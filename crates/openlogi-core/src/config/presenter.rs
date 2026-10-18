//! A presenter's force-sensing highlight button: [`PresenterSettings`] and its
//! validated [`ForceThreshold`].

use nutype::nutype;
use serde::{Deserialize, Serialize};

use super::device::is_true;

const FORCE_THRESHOLD_MIN: u16 = 93;
const FORCE_THRESHOLD_MAX: u16 = 933;

/// A force threshold of a presenter's button, in the device's raw force units
/// (`93..=933` on a Spotlight 2); lower means a lighter touch.
///
/// The range is what a Spotlight 2 reports in its `0x19c0` capability reply,
/// so a stored value is valid for the config; the device layer separately
/// checks a threshold against the range the connected device reports now.
#[nutype(
    const_fn,
    validate(greater_or_equal = FORCE_THRESHOLD_MIN, less_or_equal = FORCE_THRESHOLD_MAX),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct ForceThreshold(u16);

impl ForceThreshold {
    /// Lightest selectable threshold.
    pub const MIN: Self = match Self::try_new(FORCE_THRESHOLD_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum force threshold"),
    };
    /// Firmest selectable threshold.
    pub const MAX: Self = match Self::try_new(FORCE_THRESHOLD_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum force threshold"),
    };
}

/// A presenter's force-sensing button settings: the force thresholds and the
/// haptics the device layer writes, together with both-levels reporting, when
/// it configures a presenter. The Spotlight 2's top button only reports its
/// touch and firm-press controls once it has been switched into both-levels
/// reporting, which Logi Options+ does at every connect.
///
/// An unset threshold means the OpenLogi default
/// ([`Self::DEFAULT_TOUCH_FORCE`] / [`Self::DEFAULT_PRESS_FORCE`]), which is
/// lighter than Logitech's own 140 / 350. The device layer writes a threshold
/// only when the device holds another value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresenterSettings {
    /// Force at which the button reports a touch (first level). Unset means
    /// [`Self::DEFAULT_TOUCH_FORCE`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touch_force: Option<ForceThreshold>,
    /// Force at which the button reports a firm press (second level). Unset
    /// means [`Self::DEFAULT_PRESS_FORCE`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub press_force: Option<ForceThreshold>,
    /// Keep the presenter's haptic feedback (HID++ `0x19b0`) enabled. `true`
    /// by default, as in Logi Options+; `false` turns it off on the device.
    #[serde(skip_serializing_if = "is_true")]
    pub haptics: bool,
}

impl PresenterSettings {
    /// The touch force used when none is configured: the lightest the
    /// Spotlight 2 accepts.
    pub const DEFAULT_TOUCH_FORCE: ForceThreshold = ForceThreshold::MIN;
    /// The firm-press force used when none is configured.
    pub const DEFAULT_PRESS_FORCE: ForceThreshold = match ForceThreshold::try_new(300) {
        Ok(value) => value,
        Err(_) => panic!("valid default press force"),
    };

    /// The touch force to write: the configured one, else the default.
    #[must_use]
    pub fn effective_touch_force(&self) -> ForceThreshold {
        self.touch_force.unwrap_or(Self::DEFAULT_TOUCH_FORCE)
    }

    /// The firm-press force to write: the configured one, else the default.
    #[must_use]
    pub fn effective_press_force(&self) -> ForceThreshold {
        self.press_force.unwrap_or(Self::DEFAULT_PRESS_FORCE)
    }
}

impl Default for PresenterSettings {
    fn default() -> Self {
        Self {
            touch_force: None,
            press_force: None,
            haptics: true,
        }
    }
}
