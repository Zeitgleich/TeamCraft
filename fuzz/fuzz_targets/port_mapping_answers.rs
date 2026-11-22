//! Routers' answers to port mapping requests (#127, NAT-PMP and PCP): anything on the local network
//! can send them, so reading must never panic, and whatever is taken must be a usable mapping.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_media::port_mapping_wire::{
  MAX_LIFETIME, MIN_LIFETIME, is_reachable_external, read_nat_pmp_external_address, read_nat_pmp_map,
  read_pcp_map,
};

const PORT: u16 = 40_000;
const NONCE: [u8; 12] = [7; 12];

fuzz_target!(|data: &[u8]| {
  if let Ok(ip) = read_nat_pmp_external_address(data) {
    assert!(is_reachable_external(ip));
  }
  for granted in [read_nat_pmp_map(data, PORT), read_pcp_map(data, PORT, &NONCE)]
    .into_iter()
    .flatten()
  {
    assert_ne!(granted.external_port, 0);
    assert!((MIN_LIFETIME..=MAX_LIFETIME).contains(&granted.lifetime));
    assert!(granted.maybe_external_ip.is_none_or(is_reachable_external));
  }
});
