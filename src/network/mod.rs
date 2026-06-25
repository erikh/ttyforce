pub mod interface;
pub mod operations;
pub mod state;
pub mod wifi;

pub use interface::NetworkInterface;
pub use state::NetworkState;
pub use wifi::WifiNetwork;

/// Public fallback DNS resolvers, in priority order. This is the single source
/// of truth for the hardcoded public resolvers — used everywhere a fallback is
/// needed (DNS-resolution candidate list, internet-routability pings, the
/// online check) so the set stays consistent. Google (`8.8.8.8`, `8.8.4.4`) is
/// listed ahead of Cloudflare (`1.1.1.1`) because some networks filter 1.1.1.1.
pub const PUBLIC_FALLBACK_DNS: [&str; 3] = ["8.8.8.8", "8.8.4.4", "1.1.1.1"];
