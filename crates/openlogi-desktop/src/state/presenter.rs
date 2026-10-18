//! Per-device presenter settings: the highlight button's effects and the
//! force-sensing thresholds of a presenter such as the Spotlight 2.
//!
//! Effects are ordinary button bindings ([`AppState::commit_binding`]);
//! thresholds and haptics live in `[devices.<key>.presenter]`. The agent
//! applies both to the device on reload and on connect, so nothing here talks
//! to hardware. Only the device's base settings are edited: a presenter has no
//! per-app profile scope.

use std::collections::BTreeMap;

use gpui::App;
use openlogi_core::binding::{Action, ButtonId, default_binding};
use openlogi_core::bindings::bindings_for;
use openlogi_core::config::PresenterSettings;
use tracing::{debug, warn};

use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::{AppState, StateEvent};

/// The actions bound to the highlight button's controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterBindings {
    /// The light touch (`ButtonId::PresenterTouch`).
    pub touch: Action,
    /// The firm press (`ButtonId::PresenterPress`).
    pub press: Action,
    /// The short click (`ButtonId::PresenterClick`).
    pub click: Action,
}

impl PresenterBindings {
    fn resolve(bound: &BTreeMap<ButtonId, Action>) -> Self {
        let of = |button| {
            bound
                .get(&button)
                .cloned()
                .unwrap_or_else(|| default_binding(button))
        };
        Self {
            touch: of(ButtonId::PresenterTouch),
            press: of(ButtonId::PresenterPress),
            click: of(ButtonId::PresenterClick),
        }
    }
}

impl Default for PresenterBindings {
    fn default() -> Self {
        Self::resolve(&BTreeMap::new())
    }
}

impl AppState {
    /// What the highlight button's three controls do on the active device's
    /// base profile, each falling back to its default. Ignores any per-app
    /// scope a window has open: the Presenter panel edits the device's own
    /// bindings only.
    #[must_use]
    pub fn current_presenter_bindings(&self) -> PresenterBindings {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key);
        PresenterBindings::resolve(&bindings_for(&self.config, key, None))
    }

    /// The active device's presenter settings, or the defaults when it has
    /// none stored.
    #[must_use]
    pub fn current_presenter_settings(&self) -> PresenterSettings {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.presenter(key))
            .unwrap_or_default()
    }

    /// Bind a highlight-button control (`PresenterTouch` / `PresenterPress` /
    /// `PresenterClick`) to `action` through the shared binding-commit path.
    pub fn commit_presenter_binding(&mut self, button: ButtonId, action: Action) -> StateEvents {
        if !button.is_presenter_button() {
            debug!(?button, "not a presenter button — binding ignored");
            return StateEvents::none();
        }
        self.commit_binding(button, action)
    }

    /// Persist the active device's presenter settings and have the agent
    /// reload them. Core owns the policy: an inverted threshold pair is
    /// refused (`validate`), and a value equal to its default is stored as
    /// unset (`normalized`), so the config stays free of values the agent
    /// would pick anyway.
    pub fn commit_presenter_settings(&mut self, settings: PresenterSettings) -> StateEvents {
        let Some(record) = self.current_record() else {
            return StateEvents::none();
        };
        let events = StateEvent::DeviceConfigChanged(record.device_key()).into();
        let Some(key) = record.persistent_config_key().map(str::to_string) else {
            debug!("no persistent device key — presenter change ignored");
            return events;
        };
        if let Err(error) = settings.validate() {
            warn!(%error, "invalid presenter settings — change ignored");
            return events;
        }
        let normalized = settings.normalized();
        if self.config.presenter(&key).unwrap_or_default() == normalized {
            return events;
        }
        self.config
            .edit(|config| config.set_presenter(&key, normalized));
        self.persist_and_reload("presenter settings");
        events
    }

    /// Read-modify-commit the active device's presenter settings: `edit`
    /// changes the stored values, every other one stays as it is.
    pub(crate) fn edit_presenter(cx: &mut App, edit: impl FnOnce(&mut PresenterSettings)) {
        Self::apply(cx, |state| {
            let mut settings = state.current_presenter_settings();
            edit(&mut settings);
            state.commit_presenter_settings(settings)
        });
    }
}
