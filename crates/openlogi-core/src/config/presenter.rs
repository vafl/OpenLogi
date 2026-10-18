//! A presenter's force-sensing highlight button: [`PresenterSettings`]
//! (`[devices.<key>.presenter]`), its validated value types, and the
//! [`HighlightAppearance`] / [`HighlightMotion`] views the agent publishes to
//! the overlay.

use nutype::nutype;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::device::is_true;
use crate::color::Rgb;

const FORCE_THRESHOLD_MIN: u16 = 93;
const FORCE_THRESHOLD_MAX: u16 = 933;
const LASER_SIZE_MIN: u16 = 4;
const LASER_SIZE_MAX: u16 = 40;
const LASER_SIZE_DEFAULT: u16 = 12;
const SPOTLIGHT_SIZE_MIN: u16 = 60;
const SPOTLIGHT_SIZE_MAX: u16 = 400;
const SPOTLIGHT_SIZE_DEFAULT: u16 = 170;
const SPOTLIGHT_DIM_MIN: u8 = 0;
const SPOTLIGHT_DIM_MAX: u8 = 100;
const SPOTLIGHT_DIM_DEFAULT: u8 = 50;
const HIGHLIGHT_SENSITIVITY_MIN: u8 = 25;
const HIGHLIGHT_SENSITIVITY_MAX: u8 = 200;
const HIGHLIGHT_SENSITIVITY_DEFAULT: u8 = 100;

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

/// Radius of the laser dot in points, `4..=40`.
#[nutype(
    const_fn,
    validate(greater_or_equal = LASER_SIZE_MIN, less_or_equal = LASER_SIZE_MAX),
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
pub struct LaserSize(u16);

impl LaserSize {
    /// Smallest selectable radius.
    pub const MIN: Self = match Self::try_new(LASER_SIZE_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum laser size"),
    };
    /// Largest selectable radius.
    pub const MAX: Self = match Self::try_new(LASER_SIZE_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum laser size"),
    };
    /// Out-of-the-box radius.
    pub const DEFAULT: Self = match Self::try_new(LASER_SIZE_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default laser size"),
    };
}

impl Default for LaserSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Radius of the spotlight circle in points, `60..=400`.
#[nutype(
    const_fn,
    validate(greater_or_equal = SPOTLIGHT_SIZE_MIN, less_or_equal = SPOTLIGHT_SIZE_MAX),
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
pub struct SpotlightSize(u16);

impl SpotlightSize {
    /// Smallest selectable radius.
    pub const MIN: Self = match Self::try_new(SPOTLIGHT_SIZE_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum spotlight size"),
    };
    /// Largest selectable radius.
    pub const MAX: Self = match Self::try_new(SPOTLIGHT_SIZE_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum spotlight size"),
    };
    /// Out-of-the-box radius.
    pub const DEFAULT: Self = match Self::try_new(SPOTLIGHT_SIZE_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default spotlight size"),
    };
}

impl Default for SpotlightSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How strongly the spotlight dims the rest of the screen, in percent,
/// `0..=100`.
#[nutype(
    const_fn,
    validate(greater_or_equal = SPOTLIGHT_DIM_MIN, less_or_equal = SPOTLIGHT_DIM_MAX),
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
pub struct SpotlightDim(u8);

impl SpotlightDim {
    /// No dimming.
    pub const MIN: Self = match Self::try_new(SPOTLIGHT_DIM_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum spotlight dimming"),
    };
    /// Fully dark outside the spotlight.
    pub const MAX: Self = match Self::try_new(SPOTLIGHT_DIM_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum spotlight dimming"),
    };
    /// Out-of-the-box dimming.
    pub const DEFAULT: Self = match Self::try_new(SPOTLIGHT_DIM_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default spotlight dimming"),
    };
}

impl Default for SpotlightDim {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How far the highlight moves per unit of device motion, in percent,
/// `25..=200`.
#[nutype(
    const_fn,
    validate(greater_or_equal = HIGHLIGHT_SENSITIVITY_MIN, less_or_equal = HIGHLIGHT_SENSITIVITY_MAX),
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
pub struct HighlightSensitivity(u8);

impl HighlightSensitivity {
    /// Lowest selectable sensitivity.
    pub const MIN: Self = match Self::try_new(HIGHLIGHT_SENSITIVITY_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum highlight sensitivity"),
    };
    /// Highest selectable sensitivity.
    pub const MAX: Self = match Self::try_new(HIGHLIGHT_SENSITIVITY_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum highlight sensitivity"),
    };
    /// Out-of-the-box sensitivity: the base gain.
    pub const DEFAULT: Self = match Self::try_new(HIGHLIGHT_SENSITIVITY_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default highlight sensitivity"),
    };
}

impl Default for HighlightSensitivity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What the visible mouse pointer does while a presenter highlight shows
/// (`pointer = "hide" | "follow"` in `[devices.<key>.presenter]`).
///
/// `Hide` is pure visualisation: the pointer is hidden from the first
/// highlight until the mouse is moved and is never moved itself, and the dot
/// resumes where it left off, like an independent laser. `Follow` moves the
/// pointer along with the dot, so a click during a highlight lands where the
/// dot is: the dot starts under the pointer and the pointer ends under the dot
/// on release.
///
/// Crosses the IPC inside `ActiveHighlight`, so variants are append-only:
/// serde encodes the declaration index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HighlightPointer {
    /// The pointer is hidden from the first highlight until the mouse moves,
    /// and is never moved; the dot resumes where it left off.
    #[default]
    Hide,
    /// The visible pointer follows the dot and ends under it on release; the
    /// dot starts under the pointer.
    Follow,
}

impl HighlightPointer {
    /// `skip_serializing_if` helper: true for the default, which stays out of
    /// `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A presenter's force-sensing button settings (`[devices.<key>.presenter]`),
/// applied by the agent whenever the presenter comes online: the Spotlight 2's
/// top button only reports its touch and firm-press controls once the agent has
/// switched it into both-levels reporting, which Logi Options+ does at every
/// connect.
///
/// An unset threshold means the OpenLogi default
/// ([`Self::DEFAULT_TOUCH_FORCE`] / [`Self::DEFAULT_PRESS_FORCE`]), which is
/// lighter than Logitech's own 140 / 350: raise them if the pointer triggers
/// while you merely hold the remote. The agent writes a threshold only when
/// the device holds another value.
///
/// Like [`SmartShift`](super::SmartShift), a value outside its documented
/// range is rejected when the config loads, and so is a touch threshold above
/// the press threshold ([`Self::validate`]); neither is silently corrected.
/// Defaults stay out of the serialized table.
///
/// Config-file only — never crosses the IPC (only [`HighlightPointer`],
/// [`HighlightAppearance`] and [`HighlightMotion`] do, inside the published
/// highlight).
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
    /// What the visible pointer does during a highlight. [`Hide`] by default,
    /// as in Logi Options+.
    ///
    /// [`Hide`]: HighlightPointer::Hide
    #[serde(skip_serializing_if = "HighlightPointer::is_default")]
    pub pointer: HighlightPointer,
    /// Radius of the laser dot in points.
    #[serde(skip_serializing_if = "is_default")]
    pub laser_size: LaserSize,
    /// Colour of the laser dot, `"rrggbb"` in the config file
    /// ([`Self::DEFAULT_LASER_COLOR`] by default).
    #[serde(skip_serializing_if = "is_default_laser_color")]
    pub laser_color: Rgb,
    /// Radius of the spotlight circle in points.
    #[serde(skip_serializing_if = "is_default")]
    pub spotlight_size: SpotlightSize,
    /// How strongly the spotlight dims the rest of the screen.
    #[serde(skip_serializing_if = "is_default")]
    pub spotlight_dim: SpotlightDim,
    /// How far the highlight moves per unit of device motion.
    #[serde(skip_serializing_if = "is_default")]
    pub sensitivity: HighlightSensitivity,
    /// Slow the highlight down while the hand moves slowly, so fine aiming
    /// is easier. `true` by default.
    #[serde(skip_serializing_if = "is_true")]
    pub precision: bool,
    /// On release, put the pointer back where it rested for a moment before
    /// a sudden flick at the end of the gesture. Releasing a button jiggles
    /// the hand, which otherwise moves the pointer off the aimed target.
    /// `true` by default.
    #[serde(skip_serializing_if = "is_true")]
    pub steady_release: bool,
}

/// The touch threshold of a [`PresenterSettings`] exceeds its press
/// threshold, so the first force level would be firmer than the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("presenter touch_force {touch} must not exceed press_force {press}")]
pub struct InvertedForceThresholds {
    /// The effective touch threshold.
    pub touch: ForceThreshold,
    /// The effective press threshold.
    pub press: ForceThreshold,
}

/// How the overlay moves a presenter highlight: the motion part of
/// [`PresenterSettings`], published with every highlight like
/// [`HighlightAppearance`].
///
/// Crosses the IPC inside `ActiveHighlight`, so fields are append-only. Built
/// by [`PresenterSettings::motion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightMotion {
    /// Percent of the base gain applied to device motion.
    pub sensitivity: u8,
    /// Whether slow motion is damped for fine aiming.
    pub precision: bool,
    /// Whether a release that follows a flick reports the resting point
    /// before it instead of the last position.
    pub steady_release: bool,
}

impl Default for HighlightMotion {
    fn default() -> Self {
        PresenterSettings::default().motion()
    }
}

/// How the overlay draws a presenter highlight: the appearance part of
/// [`PresenterSettings`], published with every highlight so the overlay never
/// reads the config itself.
///
/// Crosses the IPC inside `ActiveHighlight`, so fields are append-only. Built
/// by [`PresenterSettings::appearance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightAppearance {
    /// Radius of the laser dot in points.
    pub laser_size: u16,
    /// Colour of the laser dot.
    pub laser_color: Rgb,
    /// Radius of the spotlight circle in points.
    pub spotlight_size: u16,
    /// Dimming outside the spotlight, in percent.
    pub spotlight_dim: u8,
}

impl Default for HighlightAppearance {
    fn default() -> Self {
        PresenterSettings::default().appearance()
    }
}

/// `skip_serializing_if` helper for a field that is left out at its type's
/// default.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// `skip_serializing_if` helper for [`PresenterSettings::laser_color`].
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_default_laser_color(value: &Rgb) -> bool {
    *value == PresenterSettings::DEFAULT_LASER_COLOR
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
    /// The laser colour used when none is configured (`ff2a2a`).
    pub const DEFAULT_LASER_COLOR: Rgb = Rgb::new(0xff, 0x2a, 0x2a);

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

    /// Check the one rule the field types cannot: the touch threshold must
    /// not exceed the press threshold. The config loader and every interactive
    /// writer share this policy; an inverted pair is rejected, never
    /// reordered, so a writer keeps its sliders ordered itself.
    ///
    /// # Errors
    ///
    /// [`InvertedForceThresholds`] when the touch threshold is firmer than
    /// the press threshold.
    pub fn validate(&self) -> Result<(), InvertedForceThresholds> {
        let (touch, press) = (self.effective_touch_force(), self.effective_press_force());
        if touch <= press {
            Ok(())
        } else {
            Err(InvertedForceThresholds { touch, press })
        }
    }

    /// The canonical stored form: a threshold equal to its default becomes
    /// unset, so equal settings compare equal and the config stays free of
    /// values the agent would pick anyway.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            touch_force: self
                .touch_force
                .filter(|&force| force != Self::DEFAULT_TOUCH_FORCE),
            press_force: self
                .press_force
                .filter(|&force| force != Self::DEFAULT_PRESS_FORCE),
            ..self
        }
    }

    /// The overlay-facing appearance.
    #[must_use]
    pub fn appearance(&self) -> HighlightAppearance {
        HighlightAppearance {
            laser_size: self.laser_size.into_inner(),
            laser_color: self.laser_color,
            spotlight_size: self.spotlight_size.into_inner(),
            spotlight_dim: self.spotlight_dim.into_inner(),
        }
    }

    /// The overlay-facing motion.
    #[must_use]
    pub fn motion(&self) -> HighlightMotion {
        HighlightMotion {
            sensitivity: self.sensitivity.into_inner(),
            precision: self.precision,
            steady_release: self.steady_release,
        }
    }
}

impl Default for PresenterSettings {
    fn default() -> Self {
        Self {
            touch_force: None,
            press_force: None,
            haptics: true,
            pointer: HighlightPointer::Hide,
            laser_size: LaserSize::DEFAULT,
            laser_color: Self::DEFAULT_LASER_COLOR,
            spotlight_size: SpotlightSize::DEFAULT,
            spotlight_dim: SpotlightDim::DEFAULT,
            sensitivity: HighlightSensitivity::DEFAULT,
            precision: true,
            steady_release: true,
        }
    }
}

/// Load a device's `presenter` table, rejecting inverted thresholds.
pub(super) fn deserialize_presenter<'de, D>(
    deserializer: D,
) -> Result<Option<PresenterSettings>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let presenter = Option::<PresenterSettings>::deserialize(deserializer)?;
    if let Some(settings) = &presenter {
        settings.validate().map_err(serde::de::Error::custom)?;
    }
    Ok(presenter)
}
