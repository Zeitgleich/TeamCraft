//! Re-runs the sqlx macros when a migration is added (`sqlx::migrate!`).

fn main() {
  println!("cargo:rerun-if-changed=migrations");
}
