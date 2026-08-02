/// Provides functionality to limit the number of active channels.
pub mod channels_per_key;

/// Provides a [channel](crate::server::Channel) that limits the number of in-flight requests.
pub mod requests_per_channel;
