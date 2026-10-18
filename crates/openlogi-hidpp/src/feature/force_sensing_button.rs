//! Implements the reverse-engineered `ForceSensingButton` feature (`0x19c0`),
//! the pressure-sensitive button of devices such as the Spotlight 2.
//!
//! The function and payload layouts follow Solaar's `ForceSensingButton` /
//! `ForceSensingButtonArray` (`hidpp20.py`). Logitech has not published this
//! feature in the public HID++ spec, so additions must be verified against
//! hardware rather than guessed. All multi-byte values are big-endian.
//!
//! A Spotlight 2 reports feature version 2 while [`ForceSensingButtonFeature`]
//! is declared `version = 0` like its neighbours: the layouts here are the
//! ones observed on that version, and no other version has been seen.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Per-button threshold capabilities (`getButtonCapabilities`).
///
/// Thresholds are raw device units; a lower value means a lighter press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForceButtonCapabilities {
    /// Whether the threshold can be changed (bit 0 of the first field).
    pub changeable: bool,
    /// Factory default of the first threshold (the reply carries a second
    /// default that is not decoded).
    pub default: u16,
    /// Highest accepted threshold.
    pub max: u16,
    /// Lowest accepted threshold.
    pub min: u16,
}

impl ForceButtonCapabilities {
    fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [c0, c1, d0, d1, x0, x1, n0, n1, ..] = *payload else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self {
            changeable: u16::from_be_bytes([c0, c1]) & 1 != 0,
            default: u16::from_be_bytes([d0, d1]),
            max: u16::from_be_bytes([x0, x1]),
            min: u16::from_be_bytes([n0, n1]),
        })
    }

    /// Whether `value` lies within `min..=max`.
    #[must_use]
    pub fn contains(&self, value: u16) -> bool {
        (self.min..=self.max).contains(&value)
    }

    /// The one rule for a threshold pair a button may be given: both levels
    /// within `min..=max` and the first no firmer than the second.
    ///
    /// # Errors
    ///
    /// [`ForceConfigRejection::OutOfRange`] when a level is outside the
    /// range, [`ForceConfigRejection::Inverted`] when the first level is
    /// firmer than the second.
    pub fn accepts(&self, config: &ForceButtonConfig) -> Result<(), ForceConfigRejection> {
        if !self.contains(config.level1) || !self.contains(config.level2) {
            Err(ForceConfigRejection::OutOfRange {
                min: self.min,
                max: self.max,
            })
        } else if config.level1 > config.level2 {
            Err(ForceConfigRejection::Inverted {
                level1: config.level1,
                level2: config.level2,
            })
        } else {
            Ok(())
        }
    }
}

/// Why [`ForceButtonCapabilities::accepts`] refused a threshold pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForceConfigRejection {
    /// A level lies outside the button's accepted range.
    #[error("thresholds must lie within {min}..={max}")]
    OutOfRange {
        /// Lowest accepted threshold.
        min: u16,
        /// Highest accepted threshold.
        max: u16,
    },
    /// The first level is firmer than the second.
    #[error("the first threshold ({level1}) must not exceed the second ({level2})")]
    Inverted {
        /// The first (lighter) threshold.
        level1: u16,
        /// The second threshold.
        level2: u16,
    },
}

/// The current thresholds of one button (`getButtonConfig`).
///
/// Observed on a Spotlight 2 (feature version 2): two big-endian `u16`s
/// (`00 a8 01 a4` = 168 and 420). Only the first is described by Solaar; the
/// second is inferred from captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForceButtonConfig {
    /// First (lighter) threshold.
    pub level1: u16,
    /// Second threshold.
    pub level2: u16,
}

impl ForceButtonConfig {
    fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [a, b, c, d, ..] = *payload else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self {
            level1: u16::from_be_bytes([a, b]),
            level2: u16::from_be_bytes([c, d]),
        })
    }
}

bitflags::bitflags! {
    /// TENTATIVE: which force levels a button reports (`getLevelReportingMode` /
    /// `setLevelReportingMode`). Reverse-engineered from a Logi Options+
    /// capture on a Spotlight 2, not from a spec: bit 0 reports force level 1
    /// (`0x1b04` CID `0x00d8`), bit 1 reports force level 2 (CID `0x01a8`).
    /// Unknown bits are retained.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ForceLevelMode: u8 {
        /// Report force level 1.
        const REPORT_LEVEL_1 = 1 << 0;
        /// Report force level 2.
        const REPORT_LEVEL_2 = 1 << 1;
        /// Both levels (`3`), the mode Options+ writes. With only
        /// `REPORT_LEVEL_1` (`1`, observed before Options+ ran) the second
        /// level is never reported.
        const BOTH = Self::REPORT_LEVEL_1.bits() | Self::REPORT_LEVEL_2.bits();
    }
}

impl ForceLevelMode {
    /// Decode the reply of `getLevelReportingMode` (function 5): the mode is
    /// payload byte 0 (`03 00 00 ...` on a Spotlight 2).
    fn from_get_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let [mode, ..] = *payload else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(Self::from_bits_retain(mode))
    }
}

/// Implements `ForceSensingButton` / `0x19c0`.
#[derive(Clone, Feature)]
#[creatable(id = 0x19c0, version = 0)]
pub struct ForceSensingButtonFeature {
    endpoint: FeatureEndpoint,
}

impl ForceSensingButtonFeature {
    /// Read the number of force-sensing buttons (`getCapabilities`,
    /// function 0).
    pub async fn get_button_count(&self) -> Result<u8, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(payload[0])
    }

    /// Read the threshold range of `button` (`getButtonCapabilities`,
    /// function 1).
    pub async fn get_button_capabilities(
        &self,
        button: u8,
    ) -> Result<ForceButtonCapabilities, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [button, 0, 0])
            .await?
            .extend_payload();
        ForceButtonCapabilities::from_payload(&payload)
    }

    /// Read the current thresholds of `button` (`getButtonConfig`,
    /// function 2).
    pub async fn get_button_config(&self, button: u8) -> Result<ForceButtonConfig, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [button, 0, 0])
            .await?
            .extend_payload();
        ForceButtonConfig::from_payload(&payload)
    }

    /// Write both thresholds of `button` (`setButtonConfig`, function 3) as
    /// a long report `[button, l1_hi, l1_lo, l2_hi, l2_lo]`. A short report
    /// with only the first threshold was rejected with `InvalidArgument` on
    /// a Spotlight 2. The reply echoes `[button, level1_hi, level1_lo,
    /// level2_hi, level2_lo]` (seen in an Options+ capture) but is not
    /// decoded; read the value back instead.
    pub async fn set_button_config(
        &self,
        button: u8,
        config: ForceButtonConfig,
    ) -> Result<(), Hidpp20Error> {
        let [a, b] = config.level1.to_be_bytes();
        let [c, d] = config.level2.to_be_bytes();
        let mut args = [0; 16];
        args[..5].copy_from_slice(&[button, a, b, c, d]);
        self.endpoint.call_long(3, args).await?;
        Ok(())
    }

    /// TENTATIVE: read which force levels `button` reports
    /// (`getLevelReportingMode`, function 5). Reverse-engineered: `1` was
    /// observed on a Spotlight 2 before Options+ ran, `3` after. The reply
    /// carries the mode in payload byte 0 (unlike function 6).
    pub async fn get_level_mode(&self, button: u8) -> Result<ForceLevelMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(5, [button, 0, 0])
            .await?
            .extend_payload();
        ForceLevelMode::from_get_payload(&payload)
    }

    /// TENTATIVE: set which force levels `button` reports
    /// (`setLevelReportingMode`, function 6) as a short report `[button,
    /// mode, 0]`. Options+ sends button 0, mode 3 and the device answers
    /// `00 03`, but the echo is not decoded: it has been observed to be stale
    /// (`mode 1` echoed 0x03 while the device then read back 0x01), so callers
    /// must confirm the applied mode with [`Self::get_level_mode`].
    pub async fn set_level_mode(
        &self,
        button: u8,
        mode: ForceLevelMode,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call(6, [button, mode.bits(), 0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::sync::Arc;

    use super::*;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    #[test]
    fn capabilities_decode_big_endian_in_wire_order() {
        let caps = ForceButtonCapabilities::from_payload(&[
            0x00, 0x01, 0x01, 0x2c, 0x03, 0xe8, 0x00, 0x64, 2, 0x01, 0x5e, 0,
        ])
        .unwrap();
        assert!(caps.changeable);
        assert_eq!((caps.default, caps.max, caps.min), (300, 1000, 100));
        assert!(caps.contains(100) && caps.contains(1000));
        assert!(!caps.contains(99) && !caps.contains(1001));
    }

    #[test]
    fn accepts_checks_range_then_order() {
        let caps = ForceButtonCapabilities {
            changeable: true,
            default: 300,
            max: 1000,
            min: 100,
        };
        let pair = |level1, level2| ForceButtonConfig { level1, level2 };
        assert_eq!(caps.accepts(&pair(100, 1000)), Ok(()));
        assert_eq!(caps.accepts(&pair(500, 500)), Ok(()));
        for out in [pair(99, 500), pair(500, 1001)] {
            assert_eq!(
                caps.accepts(&out),
                Err(ForceConfigRejection::OutOfRange {
                    min: 100,
                    max: 1000
                })
            );
        }
        assert_eq!(
            caps.accepts(&pair(600, 500)),
            Err(ForceConfigRejection::Inverted {
                level1: 600,
                level2: 500
            })
        );
    }

    #[test]
    fn only_bit_zero_makes_a_button_changeable() {
        let caps =
            ForceButtonCapabilities::from_payload(&[0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
                .unwrap();
        assert!(!caps.changeable);
    }

    #[test]
    fn short_capabilities_are_rejected() {
        assert_matches!(
            ForceButtonCapabilities::from_payload(&[0; 7]),
            Err(Hidpp20Error::UnsupportedResponse)
        );
    }

    #[test]
    fn config_decodes_two_thresholds() {
        let c = ForceButtonConfig::from_payload(&[0x00, 0xa8, 0x01, 0xa4, 0]).unwrap();
        assert_eq!((c.level1, c.level2), (168, 420));
    }

    #[test]
    fn get_level_mode_reads_byte_zero() {
        let both = ForceLevelMode::from_get_payload(&[0x03, 0, 0]).unwrap();
        assert_eq!(both, ForceLevelMode::BOTH);
        let one = ForceLevelMode::from_get_payload(&[0x01, 0]).unwrap();
        assert_eq!(one, ForceLevelMode::REPORT_LEVEL_1);
        assert!(!one.contains(ForceLevelMode::REPORT_LEVEL_2));
        assert_eq!(
            ForceLevelMode::from_get_payload(&[0x80]).unwrap().bits(),
            0x80
        );
        assert_matches!(
            ForceLevelMode::from_get_payload(&[]),
            Err(Hidpp20Error::UnsupportedResponse)
        );
    }

    /// Run `call` against a mock device that answers with a short report
    /// `reply` for `function`; returns the call's result and the request the
    /// device saw.
    fn with_reply<T>(
        function: u8,
        reply: [u8; 3],
        call: impl for<'a> FnOnce(
            &'a ForceSensingButtonFeature,
        ) -> std::pin::Pin<Box<dyn Future<Output = T> + 'a>>,
    ) -> (T, Vec<u8>) {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = ForceSensingButtonFeature::new(channel, 3, 0x0b);
            let mut pending = call(&feature);
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 0x0b,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, reply).into())
                .await;
            let result = pending.await;
            let request = handle
                .written_reports()
                .last()
                .cloned()
                .expect("a request was sent");
            (result, request)
        })
    }

    #[test]
    fn get_button_count_reads_payload_byte_zero() {
        let (count, request) = with_reply(0, [2, 0, 0], |f| Box::pin(f.get_button_count()));
        assert_eq!(count.unwrap(), 2);
        assert_eq!(request[3] >> 4, 0, "getCapabilities is function 0");
    }

    #[test]
    fn set_button_config_sends_one_long_report_with_both_levels() {
        let config = ForceButtonConfig {
            level1: 168,
            level2: 420,
        };
        let (result, request) = with_reply(3, [0, 0, 0xa8], |f| {
            Box::pin(f.set_button_config(1, config))
        });
        result.unwrap();
        assert_eq!(request[0], 0x11, "a long report");
        assert_eq!(request[3] >> 4, 3, "setButtonConfig is function 3");
        assert_eq!(&request[4..9], &[1, 0x00, 0xa8, 0x01, 0xa4]);
    }

    #[test]
    fn get_level_mode_sends_a_short_report_and_reads_reply_byte_zero() {
        let (mode, request) = with_reply(5, [0x03, 0, 0], |f| Box::pin(f.get_level_mode(1)));
        assert_eq!(mode.unwrap(), ForceLevelMode::BOTH);
        assert_eq!(request[0], 0x10, "a short report");
        assert_eq!(request[3] >> 4, 5, "getLevelReportingMode is function 5");
        assert_eq!(&request[4..7], &[1, 0, 0]);
    }

    #[test]
    fn set_level_mode_sends_button_and_mode() {
        let (result, request) = with_reply(6, [0, 0x03, 0], |f| {
            Box::pin(f.set_level_mode(0, ForceLevelMode::BOTH))
        });
        result.unwrap();
        assert_eq!(request[0], 0x10, "a short report");
        assert_eq!(request[3] >> 4, 6, "setLevelReportingMode is function 6");
        assert_eq!(&request[4..7], &[0, 0x03, 0]);
    }
}
