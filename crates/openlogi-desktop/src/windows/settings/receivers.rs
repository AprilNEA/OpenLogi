//! Receivers settings page: every pairing slot of each connected receiver —
//! which device sits in it, whether it is connected, and which slots are
//! empty or hold a leftover pairing that can be removed on its own.

use openlogi_core::hid::ReceiverBrand;

use super::assets::action_button;
use super::{
    AppState, Icon, ParentElement, SettingField, SettingGroup, SettingItem, SettingPage,
    SharedString, Styled, div, h_flex,
};
use crate::state::{ReceiverSlot, ReceiverSlots, SlotOccupant};
use crate::ui::theme::{Palette, Typography as _};

/// The Receivers page for the receivers `AppState` currently lists.
pub(super) fn receivers_page(receivers: &[ReceiverSlots]) -> SettingPage {
    let page = SettingPage::new(tr!("receivers.receivers"))
        .icon(Icon::empty().path("action-icons/bolt.svg"))
        .description(tr!("receivers.receivers_description"))
        .resettable(false);
    if receivers.is_empty() {
        return page.group(
            SettingGroup::new().item(
                SettingItem::new(
                    tr!("receivers.no_receiver"),
                    SettingField::render(|_, _, _| div()),
                )
                .description(tr!("receivers.no_receiver_description")),
            ),
        );
    }
    page.groups(
        receivers
            .iter()
            .enumerate()
            .map(|(index, receiver)| receiver_group(index, receiver)),
    )
}

fn receiver_group(index: usize, receiver: &ReceiverSlots) -> SettingGroup {
    let used = receiver.used().to_string();
    let usage = match receiver.capacity {
        Some(total) => {
            tr!("receivers.slots_used", used => used, total => total.to_string())
        }
        None => tr!("receivers.slots_occupied", used => used),
    };
    SettingGroup::new()
        .title(receiver_title(receiver))
        .description(usage)
        .items(
            receiver
                .slots
                .iter()
                .map(|slot| slot_item(index, slot.clone())),
        )
}

/// The receiver's family name, or what the agent called it when the family
/// has no name of its own here.
fn receiver_title(receiver: &ReceiverSlots) -> SharedString {
    match receiver.brand {
        Some(ReceiverBrand::Bolt) => tr!("device.bolt_receiver"),
        Some(ReceiverBrand::Unifying) => tr!("device.unifying_receiver"),
        Some(ReceiverBrand::Nano | ReceiverBrand::Lightspeed) | None => {
            receiver.name.clone().into()
        }
    }
}

fn slot_item(receiver_index: usize, slot: ReceiverSlot) -> SettingItem {
    let title = tr!("device.receiver_slot_number", number => slot.slot.to_string());
    SettingItem::new(
        title,
        SettingField::render(move |_, _, cx| {
            let pal = crate::ui::theme::palette(cx);
            occupant_field(receiver_index, slot.slot, &slot.occupant, pal)
        }),
    )
}

fn occupant_field(
    receiver_index: usize,
    slot: u8,
    occupant: &SlotOccupant,
    pal: Palette,
) -> gpui::Div {
    let row = h_flex().items_center().gap_2().text_body();
    match occupant {
        SlotOccupant::Empty => row
            .text_color(pal.text_muted)
            .child(tr!("receivers.empty_slot")),
        SlotOccupant::Device { name, online } => row
            .child(div().text_color(pal.text_primary).child(name.clone()))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(if *online {
                        tr!("device.connected")
                    } else {
                        tr!("device.offline")
                    }),
            ),
        SlotOccupant::OldPairing { name, route } => {
            let (name, route) = (name.clone(), route.clone());
            row.child(
                div()
                    .text_color(pal.text_muted)
                    .child(tr!("receivers.old_pairing_of", name => name.clone())),
            )
            .child(action_button(
                SharedString::from(format!("receiver-{receiver_index}-unpair-{slot}")),
                tr!("receivers.unpair"),
                pal,
                move |cx| {
                    AppState::apply(cx, |state| state.remove_stale_pairing(route.clone(), &name));
                },
            ))
        }
    }
}
