//! One module per handler, named after it (api-conventions.md).

mod get_me_handler;
mod healthz_handler;
mod readyz_handler;

pub use get_me_handler::get_me_handler;
pub use healthz_handler::healthz_handler;
pub use readyz_handler::readyz_handler;
