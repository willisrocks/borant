//! Secret store adapters. `EnvStore` reads the environment of the host
//! process and `FileStore` reads files under a root; `open_store` builds one
//! from a `[secrets.stores]` entry. A resolve never puts a value in a Debug
//! output or in an error, only a ref.

mod config;
mod env;
mod file;

pub use config::{StoreConfig, open_store};
pub use env::EnvStore;
pub use file::FileStore;
