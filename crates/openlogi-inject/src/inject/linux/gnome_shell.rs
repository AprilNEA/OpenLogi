//! GNOME Shell actions with one deadline covering connection setup and calls.

use std::cell::Cell;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use async_io::Timer;
use futures_lite::future;
use zbus::connection::Builder;
use zbus::{Connection, Proxy};

const OPERATION_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeliveryState {
    NotSent,
    MayHaveExecuted,
}

#[derive(Debug)]
pub(super) struct ActionError {
    delivery: DeliveryState,
    detail: String,
}

impl ActionError {
    fn new(detail: String, delivery: DeliveryState) -> Self {
        Self { delivery, detail }
    }

    pub(super) fn fallback_is_safe(&self) -> bool {
        self.delivery == DeliveryState::NotSent
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

pub(super) fn toggle_overview() -> Result<(), ActionError> {
    let delivery = Cell::new(DeliveryState::NotSent);
    run_bounded(toggle_overview_inner(&delivery), &delivery)
}

pub(super) fn show_applications() -> Result<(), ActionError> {
    let delivery = Cell::new(DeliveryState::NotSent);
    run_bounded(show_applications_inner(&delivery), &delivery)
}

fn run_bounded<F>(operation: F, delivery: &Cell<DeliveryState>) -> Result<(), ActionError>
where
    F: Future<Output = zbus::Result<()>>,
{
    run_before(operation, delivery, OPERATION_TIMEOUT)
}

fn run_before<F>(
    operation: F,
    delivery: &Cell<DeliveryState>,
    timeout: Duration,
) -> Result<(), ActionError>
where
    F: Future<Output = zbus::Result<()>>,
{
    match zbus::block_on(complete_before(operation, timeout)) {
        Some(result) => result.map_err(|error| ActionError::new(error.to_string(), delivery.get())),
        None => Err(ActionError::new(
            format!("operation timed out after {timeout:?}"),
            delivery.get(),
        )),
    }
}

async fn complete_before<F>(operation: F, timeout: Duration) -> Option<F::Output>
where
    F: Future,
{
    future::race(async move { Some(operation.await) }, async move {
        Timer::after(timeout).await;
        None
    })
    .await
}

async fn connect() -> zbus::Result<Connection> {
    Builder::session()?.build().await
}

async fn shell_proxy(connection: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell",
        "org.gnome.Shell",
    )
    .await
}

async fn properties_proxy(connection: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell",
        "org.freedesktop.DBus.Properties",
    )
    .await
}

async fn toggle_overview_inner(delivery: &Cell<DeliveryState>) -> zbus::Result<()> {
    let connection = connect().await?;
    let active = shell_proxy(&connection)
        .await?
        .get_property::<bool>("OverviewActive")
        .await?;
    let proxy = properties_proxy(&connection).await?;
    delivery.set(DeliveryState::MayHaveExecuted);
    proxy
        .call::<_, _, ()>(
            "Set",
            &(
                "org.gnome.Shell",
                "OverviewActive",
                zbus::zvariant::Value::new(!active),
            ),
        )
        .await
}

async fn show_applications_inner(delivery: &Cell<DeliveryState>) -> zbus::Result<()> {
    let connection = connect().await?;
    let proxy = shell_proxy(&connection).await?;
    delivery.set(DeliveryState::MayHaveExecuted);
    proxy.call::<_, _, ()>("ShowApplications", &()).await
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future;
    use std::time::Duration;

    use super::{DeliveryState, run_before};

    #[test]
    fn connection_timeout_allows_keyboard_fallback() {
        let delivery = Cell::new(DeliveryState::NotSent);
        let error = run_before(
            future::pending::<zbus::Result<()>>(),
            &delivery,
            Duration::from_millis(1),
        )
        .expect_err("the pending operation should time out");

        assert!(error.fallback_is_safe());
    }

    #[test]
    fn timeout_after_side_effect_suppresses_keyboard_fallback() {
        let delivery = Cell::new(DeliveryState::NotSent);
        let operation = async {
            delivery.set(DeliveryState::MayHaveExecuted);
            future::pending::<zbus::Result<()>>().await
        };
        let error = run_before(operation, &delivery, Duration::from_millis(1))
            .expect_err("the pending operation should time out");

        assert!(!error.fallback_is_safe());
    }
}
