//! Presenter configuration: the `0x19c0` ForceSensingButton level-reporting
//! mode and thresholds, and the `0x19b0` haptic enable flag.
//!
//! A Spotlight 2 reports its force-sensitive top button as two `0x1b04`
//! controls (touch and firm press) only while the `0x19c0` level mode asks
//! for both levels; Logi Options+ writes that mode at every connect, and so
//! does the agent, through [`apply_presenter_settings_on`]. Every step reads
//! first and writes only a difference, so applying on each reconnect is
//! quiet and idempotent.

use std::{sync::Arc, time::Duration};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, force_sensing_button::ForceSensingButtonFeature,
        haptic_feedback::HapticFeedbackFeature,
    },
};
use openlogi_core::config::PresenterSettings;
use tracing::debug;

use super::haptic::set_haptics_enabled;
use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

pub use hidpp::feature::force_sensing_button::{
    ForceButtonCapabilities, ForceButtonConfig, ForceConfigRejection, ForceLevelMode,
};

const OPERATION: HidppOperation = HidppOperation::ConfigurePresenter;

/// How long to wait before re-reading a level mode that did not read back as
/// written. Right after a connect the firmware answers the read-back with the
/// stale mode (the write lands a moment later), so one delayed re-read decides.
/// The lag seen on a Spotlight 2 is about 250 ms; twice that leaves the
/// re-read a margin instead of racing the firmware.
const READ_BACK_SETTLE: Duration = Duration::from_millis(500);

/// What [`apply_presenter_settings_on`] did to the level reporting mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelModeApplied {
    /// Every button already reported both force levels.
    Unchanged,
    /// At least one button was switched to report both force levels.
    Written,
}

/// What [`apply_presenter_settings_on`] did to the force thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdsApplied {
    /// The device already held the configured thresholds.
    Unchanged,
    /// The thresholds were written.
    Written,
    /// The device reports its thresholds as not changeable.
    NotChangeable,
    /// The configured thresholds do not fit the device's range, so nothing was
    /// written.
    OutOfRange {
        /// Lowest threshold the device accepts.
        min: u16,
        /// Highest threshold the device accepts.
        max: u16,
    },
    /// The configured touch force is above the press force, so nothing was
    /// written.
    Inverted {
        /// The configured touch threshold.
        touch: u16,
        /// The configured press threshold.
        press: u16,
    },
}

/// What [`apply_presenter_settings_on`] did to the `0x19b0` haptic enable flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HapticsApplied {
    /// The device has no haptic feedback feature.
    Unsupported,
    /// The device already matched the config: haptics were already enabled
    /// (or disabled), or the config asks for them on and the user's intensity
    /// is `0` ("off"), which is left alone.
    Unchanged,
    /// Haptics were enabled, keeping the device's intensity.
    Enabled,
    /// Haptics were disabled, keeping the device's intensity.
    Disabled,
}

/// The outcome of one [`apply_presenter_settings_on`] pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenterApplied {
    /// The level reporting mode.
    pub level_mode: LevelModeApplied,
    /// The force thresholds.
    pub thresholds: ThresholdsApplied,
    /// The haptic enable flag. Its failure leaves the force configuration
    /// above in place, so it is reported apart from them.
    pub haptics: Result<HapticsApplied, WriteError>,
}

/// Put a presenter on both-levels reporting, write the configured force
/// thresholds and set its haptics on or off as configured, writing only what
/// differs. Both force writes and the haptics write are read back. Enabling
/// haptics follows [`ensure_haptics_armed_on`]'s policy, and neither direction
/// touches the Actions Ring's cached haptic location.
///
/// [`ensure_haptics_armed_on`]: super::ensure_haptics_armed_on
///
/// # Errors
///
/// [`WriteError::FeatureUnsupported`] (`0x19c0`) for a device without the
/// force-sensing button — the caller's cue to leave the device alone — the
/// HID++ error of a failed force-button read or write,
/// [`WriteError::ForceLevelModeNotApplied`] and
/// [`WriteError::ForceThresholdsNotApplied`] for a write the device did not
/// keep. A haptics failure is carried in [`PresenterApplied::haptics`]
/// instead.
pub async fn apply_presenter_settings_on(
    shared: &SharedChannel,
    settings: PresenterSettings,
) -> Result<PresenterApplied, WriteError> {
    apply_on_channel(shared.channel(), shared.device_index(), settings).await
}

async fn apply_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    settings: PresenterSettings,
) -> Result<PresenterApplied, WriteError> {
    let force = open_force(channel, index).await?;
    let (level_mode, thresholds) = apply_force(&force, settings).await?;
    let haptics = match set_haptics_enabled(channel, index, settings.haptics).await {
        Ok(false) => Ok(HapticsApplied::Unchanged),
        Ok(true) if settings.haptics => Ok(HapticsApplied::Enabled),
        Ok(true) => Ok(HapticsApplied::Disabled),
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == HapticFeedbackFeature::ID =>
        {
            Ok(HapticsApplied::Unsupported)
        }
        Err(error) => Err(error),
    };
    Ok(PresenterApplied {
        level_mode,
        thresholds,
        haptics,
    })
}

async fn open_force(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<ForceSensingButtonFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<ForceSensingButtonFeature>(&mut device).await
}

fn force_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, OPERATION, ForceSensingButtonFeature::ID)
}

async fn apply_force(
    force: &ForceSensingButtonFeature,
    settings: PresenterSettings,
) -> Result<(LevelModeApplied, ThresholdsApplied), WriteError> {
    let buttons = force.get_button_count().await.map_err(force_error)?;
    let mut level_mode = LevelModeApplied::Unchanged;
    let mut thresholds = ThresholdsApplied::Unchanged;
    for button in 0..buttons {
        let mode = force.get_level_mode(button).await.map_err(force_error)?;
        if !mode.contains(ForceLevelMode::BOTH) {
            write_level_mode(force, button, mode | ForceLevelMode::BOTH).await?;
            debug!(button, from = mode.bits(), "force level reporting enabled");
            level_mode = LevelModeApplied::Written;
        }
        match apply_thresholds(force, button, settings).await? {
            ThresholdsApplied::Unchanged => {}
            other => thresholds = other,
        }
    }
    Ok((level_mode, thresholds))
}

async fn apply_thresholds(
    force: &ForceSensingButtonFeature,
    button: u8,
    settings: PresenterSettings,
) -> Result<ThresholdsApplied, WriteError> {
    let capabilities = force
        .get_button_capabilities(button)
        .await
        .map_err(force_error)?;
    if !capabilities.changeable {
        return Ok(ThresholdsApplied::NotChangeable);
    }
    let wanted = ForceButtonConfig {
        level1: settings.effective_touch_force().into(),
        level2: settings.effective_press_force().into(),
    };
    match capabilities.accepts(&wanted) {
        Ok(()) => {}
        Err(ForceConfigRejection::OutOfRange { min, max }) => {
            return Ok(ThresholdsApplied::OutOfRange { min, max });
        }
        Err(ForceConfigRejection::Inverted { level1, level2 }) => {
            return Ok(ThresholdsApplied::Inverted {
                touch: level1,
                press: level2,
            });
        }
    }
    let current = force.get_button_config(button).await.map_err(force_error)?;
    if wanted == current {
        return Ok(ThresholdsApplied::Unchanged);
    }
    write_thresholds(force, button, wanted).await?;
    Ok(ThresholdsApplied::Written)
}

/// Switch `button` to level-reporting mode `wanted` and return the mode the
/// device reports afterwards.
async fn write_level_mode(
    force: &ForceSensingButtonFeature,
    button: u8,
    wanted: ForceLevelMode,
) -> Result<ForceLevelMode, WriteError> {
    // The set reply's mode byte is not trusted (observed echoing the previous
    // mode), so the write is confirmed by reading it back.
    force
        .set_level_mode(button, wanted)
        .await
        .map_err(force_error)?;
    let mut after = force.get_level_mode(button).await.map_err(force_error)?;
    if after != wanted {
        // A mismatch is treated as transient: give the device a moment and
        // read once more before calling the write failed.
        tokio::time::sleep(READ_BACK_SETTLE).await;
        after = force.get_level_mode(button).await.map_err(force_error)?;
    }
    if after != wanted {
        return Err(WriteError::ForceLevelModeNotApplied {
            requested: wanted.bits(),
            actual: after.bits(),
        });
    }
    Ok(after)
}

/// Write both thresholds of `button` and return the thresholds the device
/// reports afterwards.
async fn write_thresholds(
    force: &ForceSensingButtonFeature,
    button: u8,
    wanted: ForceButtonConfig,
) -> Result<ForceButtonConfig, WriteError> {
    force
        .set_button_config(button, wanted)
        .await
        .map_err(force_error)?;
    // The set reply is not decoded, so the write is confirmed by reading it
    // back.
    let after = force.get_button_config(button).await.map_err(force_error)?;
    if after != wanted {
        return Err(WriteError::ForceThresholdsNotApplied {
            requested_touch: wanted.level1,
            requested_press: wanted.level2,
            actual_touch: after.level1,
            actual_press: after.level2,
        });
    }
    Ok(after)
}

/// What a presenter reports about one force-sensing button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForceButtonState {
    /// The thresholds the button accepts and whether they can change.
    pub capabilities: ForceButtonCapabilities,
    /// The current touch and press thresholds.
    pub thresholds: ForceButtonConfig,
    /// Which force levels the button reports.
    pub level_mode: ForceLevelMode,
}

/// Read every force-sensing button of the device `route` reaches: one entry
/// per button, in button order.
///
/// # Errors
///
/// [`WriteError::FeatureUnsupported`] (`0x19c0`) for a device without the
/// force-sensing button, or the HID++ error of a failed read.
pub async fn read_force_buttons(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<ForceButtonState>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let force = open_force(&channel, index).await?;
        let buttons = force.get_button_count().await.map_err(force_error)?;
        let mut states = Vec::with_capacity(usize::from(buttons));
        for button in 0..buttons {
            states.push(ForceButtonState {
                capabilities: force
                    .get_button_capabilities(button)
                    .await
                    .map_err(force_error)?,
                thresholds: force.get_button_config(button).await.map_err(force_error)?,
                level_mode: force.get_level_mode(button).await.map_err(force_error)?,
            });
        }
        Ok(states)
    })
    .await
}

/// Write both force thresholds of `button` on the device `route` reaches and
/// return the thresholds it reports afterwards. The values are sent as given:
/// checking them with [`ForceButtonCapabilities::accepts`] is the caller's job.
///
/// # Errors
///
/// [`WriteError::ForceThresholdsNotApplied`] when the device reads back other
/// thresholds, or the HID++ error of a failed write or read.
pub async fn set_force_thresholds(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    button: u8,
    thresholds: ForceButtonConfig,
) -> Result<ForceButtonConfig, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let force = open_force(&channel, index).await?;
        write_thresholds(&force, button, thresholds).await
    })
    .await
}

/// Set which force levels `button` reports on the device `route` reaches and
/// return the mode it reports afterwards, read back once more after a short
/// settle when the first read still shows the old mode.
///
/// # Errors
///
/// [`WriteError::ForceLevelModeNotApplied`] when the device still reports
/// another mode, or the HID++ error of a failed write or read.
pub async fn set_force_level_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    button: u8,
    mode: ForceLevelMode,
) -> Result<ForceLevelMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let force = open_force(&channel, index).await?;
        write_level_mode(&force, button, mode).await
    })
    .await
}

#[cfg(test)]
mod tests;
