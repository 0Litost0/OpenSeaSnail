//! Platform capability adapters.
//!
//! Business orchestration depends on semantic ports from `seasnail-desktop-core`;
//! OS SDK calls stay in this namespace or its platform-specific children.

pub(crate) mod clipboard;
pub(crate) mod correction_learner;
pub(crate) mod daemon;
pub(crate) mod injection;
#[allow(dead_code)]
pub(crate) mod observation;
pub(crate) mod permission;
pub(crate) mod recording;
pub(crate) mod resource;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
