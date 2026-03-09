// Used by tests now and will be wired into command handling in Tasks 3-6.
#![allow(dead_code)]

/// Custom MAV_CMD_USER_1 subcommand IDs (31011-31049).
///
/// These match the Python `MavCmdUser1Subcmd` enum used by connection_balancer.
/// They are encoded in the `param1` field of a COMMAND_LONG message with
/// command = MAV_CMD_USER_1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum MavCmdUser1SubCmd {
    /// Repeater: turn on LM
    TurnOnLm = 31011,
    /// Drone: switch to LTE
    SwitchToLte = 31012,
    /// Drone: switch to LM
    SwitchToLm = 31013,
    /// LTE radio telemetry message
    LteRadioTelemetry = 31014,
    /// LTE IP telemetry message
    LteIpTelemetry = 31015,
    /// Drone LM radio telemetry message
    DroneLmRadioTelemetry = 31016,
    /// Drone LM IP telemetry message
    DroneLmIpTelemetry = 31017,
    /// Repeater LM radio telemetry message
    RepeaterLmRadioTelemetry = 31018,
    /// Repeater LM IP telemetry message
    RepeaterLmIpTelemetry = 31019,
    /// QGC active vehicle
    ActiveVehicle = 31020,
    /// Change video quality
    VideoQuality = 31021,
    /// Switch camera type
    SwitchCamera = 31022,
    /// GPS manager
    GpsManager = 31030,
    /// LTE IP telemetry neighbours message (slot 0)
    LteIpTelemetryNeighbors0 = 31040,
    /// LTE IP telemetry neighbours message (slot 1)
    LteIpTelemetryNeighbors1 = 31041,
    /// LTE IP telemetry neighbours message (slot 2)
    LteIpTelemetryNeighbors2 = 31042,
    /// LTE IP telemetry neighbours message (slot 3)
    LteIpTelemetryNeighbors3 = 31043,
    /// LTE IP telemetry neighbours message (slot 4)
    LteIpTelemetryNeighbors4 = 31044,
    /// LTE IP telemetry neighbours message (slot 5)
    LteIpTelemetryNeighbors5 = 31045,
    /// LTE IP telemetry neighbours message (slot 6)
    LteIpTelemetryNeighbors6 = 31046,
    /// LTE IP telemetry neighbours message (slot 7)
    LteIpTelemetryNeighbors7 = 31047,
    /// LTE IP telemetry neighbours message (slot 8)
    LteIpTelemetryNeighbors8 = 31048,
    /// LTE IP telemetry neighbours message (slot 9)
    LteIpTelemetryNeighbors9 = 31049,
}

impl MavCmdUser1SubCmd {
    /// Convert a raw u16 ID to the corresponding subcommand variant.
    pub fn from_id(id: u16) -> Option<Self> {
        match id {
            31011 => Some(Self::TurnOnLm),
            31012 => Some(Self::SwitchToLte),
            31013 => Some(Self::SwitchToLm),
            31014 => Some(Self::LteRadioTelemetry),
            31015 => Some(Self::LteIpTelemetry),
            31016 => Some(Self::DroneLmRadioTelemetry),
            31017 => Some(Self::DroneLmIpTelemetry),
            31018 => Some(Self::RepeaterLmRadioTelemetry),
            31019 => Some(Self::RepeaterLmIpTelemetry),
            31020 => Some(Self::ActiveVehicle),
            31021 => Some(Self::VideoQuality),
            31022 => Some(Self::SwitchCamera),
            31030 => Some(Self::GpsManager),
            31040 => Some(Self::LteIpTelemetryNeighbors0),
            31041 => Some(Self::LteIpTelemetryNeighbors1),
            31042 => Some(Self::LteIpTelemetryNeighbors2),
            31043 => Some(Self::LteIpTelemetryNeighbors3),
            31044 => Some(Self::LteIpTelemetryNeighbors4),
            31045 => Some(Self::LteIpTelemetryNeighbors5),
            31046 => Some(Self::LteIpTelemetryNeighbors6),
            31047 => Some(Self::LteIpTelemetryNeighbors7),
            31048 => Some(Self::LteIpTelemetryNeighbors8),
            31049 => Some(Self::LteIpTelemetryNeighbors9),
            _ => None,
        }
    }

    /// Get the raw u16 ID for this subcommand.
    pub fn id(self) -> u16 {
        self as u16
    }

    /// Return all subcommand variants.
    pub fn all() -> &'static [Self] {
        &[
            Self::TurnOnLm,
            Self::SwitchToLte,
            Self::SwitchToLm,
            Self::LteRadioTelemetry,
            Self::LteIpTelemetry,
            Self::DroneLmRadioTelemetry,
            Self::DroneLmIpTelemetry,
            Self::RepeaterLmRadioTelemetry,
            Self::RepeaterLmIpTelemetry,
            Self::ActiveVehicle,
            Self::VideoQuality,
            Self::SwitchCamera,
            Self::GpsManager,
            Self::LteIpTelemetryNeighbors0,
            Self::LteIpTelemetryNeighbors1,
            Self::LteIpTelemetryNeighbors2,
            Self::LteIpTelemetryNeighbors3,
            Self::LteIpTelemetryNeighbors4,
            Self::LteIpTelemetryNeighbors5,
            Self::LteIpTelemetryNeighbors6,
            Self::LteIpTelemetryNeighbors7,
            Self::LteIpTelemetryNeighbors8,
            Self::LteIpTelemetryNeighbors9,
        ]
    }
}

impl std::fmt::Display for MavCmdUser1SubCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}({})", self, self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_id_valid() {
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31011),
            Some(MavCmdUser1SubCmd::TurnOnLm)
        );
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31012),
            Some(MavCmdUser1SubCmd::SwitchToLte)
        );
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31013),
            Some(MavCmdUser1SubCmd::SwitchToLm)
        );
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31022),
            Some(MavCmdUser1SubCmd::SwitchCamera)
        );
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31030),
            Some(MavCmdUser1SubCmd::GpsManager)
        );
        assert_eq!(
            MavCmdUser1SubCmd::from_id(31049),
            Some(MavCmdUser1SubCmd::LteIpTelemetryNeighbors9)
        );
    }

    #[test]
    fn test_from_id_invalid() {
        assert_eq!(MavCmdUser1SubCmd::from_id(0), None);
        assert_eq!(MavCmdUser1SubCmd::from_id(31010), None);
        assert_eq!(MavCmdUser1SubCmd::from_id(31023), None);
        assert_eq!(MavCmdUser1SubCmd::from_id(31029), None);
        assert_eq!(MavCmdUser1SubCmd::from_id(31050), None);
        assert_eq!(MavCmdUser1SubCmd::from_id(u16::MAX), None);
    }

    #[test]
    fn test_id_roundtrip() {
        for cmd in MavCmdUser1SubCmd::all() {
            let id = cmd.id();
            let recovered = MavCmdUser1SubCmd::from_id(id);
            assert_eq!(recovered, Some(*cmd), "roundtrip failed for {cmd}");
        }
    }

    #[test]
    fn test_all_returns_all_variants() {
        let all = MavCmdUser1SubCmd::all();
        assert_eq!(all.len(), 23);
        // Verify no duplicates
        let mut ids: Vec<u16> = all.iter().map(|c| c.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 23);
    }

    #[test]
    fn test_id_values_match_python() {
        // Verify exact ID values match the Python MavCmdUser1Subcmd enum
        assert_eq!(MavCmdUser1SubCmd::TurnOnLm.id(), 31011);
        assert_eq!(MavCmdUser1SubCmd::SwitchToLte.id(), 31012);
        assert_eq!(MavCmdUser1SubCmd::SwitchToLm.id(), 31013);
        assert_eq!(MavCmdUser1SubCmd::LteRadioTelemetry.id(), 31014);
        assert_eq!(MavCmdUser1SubCmd::LteIpTelemetry.id(), 31015);
        assert_eq!(MavCmdUser1SubCmd::DroneLmRadioTelemetry.id(), 31016);
        assert_eq!(MavCmdUser1SubCmd::DroneLmIpTelemetry.id(), 31017);
        assert_eq!(MavCmdUser1SubCmd::RepeaterLmRadioTelemetry.id(), 31018);
        assert_eq!(MavCmdUser1SubCmd::RepeaterLmIpTelemetry.id(), 31019);
        assert_eq!(MavCmdUser1SubCmd::ActiveVehicle.id(), 31020);
        assert_eq!(MavCmdUser1SubCmd::VideoQuality.id(), 31021);
        assert_eq!(MavCmdUser1SubCmd::SwitchCamera.id(), 31022);
        assert_eq!(MavCmdUser1SubCmd::GpsManager.id(), 31030);
        assert_eq!(MavCmdUser1SubCmd::LteIpTelemetryNeighbors0.id(), 31040);
        assert_eq!(MavCmdUser1SubCmd::LteIpTelemetryNeighbors9.id(), 31049);
    }

    #[test]
    fn test_display() {
        let cmd = MavCmdUser1SubCmd::SwitchToLte;
        let s = format!("{cmd}");
        assert_eq!(s, "SwitchToLte(31012)");
    }
}
