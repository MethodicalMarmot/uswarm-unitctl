use std::sync::Arc;
use std::time::Duration;

use crate::mavlink::MavFrame;
use mavlink::ardupilotmega::*;
use mavlink::MavHeader;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use super::commands::MavCmdUser1SubCmd;
use crate::context::Context;
use crate::sensors::lte::LteReading;
use crate::sensors::ping::PingReading;
use crate::Task;

pub struct TelemetryReporter {
    ctx: Arc<Context>,
    cancel: CancellationToken,
}

impl TelemetryReporter {
    pub fn new(ctx: Arc<Context>, cancel: CancellationToken) -> Self {
        Self { ctx, cancel }
    }

    /// Run the telemetry reporter loop at 1Hz.
    ///
    /// Reads the latest LTE and ping sensor values from Context, constructs
    /// COMMAND_LONG messages, and sends them to both GCS and base station.
    pub async fn run_loop(&self) {
        let self_sysid = self.ctx.config.mavlink.self_sysid;
        let self_compid = self.ctx.config.mavlink.self_compid;
        let gcs_sysid = self.ctx.config.mavlink.gcs_sysid;
        let bs_sysid = self.ctx.config.mavlink.bs_sysid;

        let mut interval = tokio::time::interval(Duration::from_secs(1));

        debug!("telemetry reporter started");

        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    debug!("telemetry reporter: shutdown");
                    return;
                }
                _ = interval.tick() => {
                    let ping = self.ctx.sensors.ping.read().await.clone().unwrap_or(PingReading::default());
                    let lte = self.ctx.sensors.lte.read().await.clone().unwrap_or(LteReading::default());

                    // Send LTE radio telemetry (subcmd 31014) to both targets
                    if !self.send_to_both(
                        gcs_sysid, bs_sysid,
                        |target_sys, target_comp| {
                            Self::build_lte_radio_telemetry(
                                self_sysid, self_compid,
                                target_sys, target_comp, &lte,
                            )
                        },
                    ).await { return; }

                    // Send LTE IP telemetry (subcmd 31015) to both targets
                    if !self.send_to_both(
                        gcs_sysid, bs_sysid,
                        |target_sys, target_comp| {
                            Self::build_lte_ip_telemetry(
                                self_sysid, self_compid,
                                target_sys, target_comp, &ping, &lte,
                            )
                        },
                    ).await { return; }

                    // Send neighbor cell telemetry (subcmds 31040-31049)
                    // Sort neighbors by pcid for deterministic ordering
                    let mut neighbor_list: Vec<_> = lte.neighbors.values().collect();
                    neighbor_list.sort_by_key(|n| n.pcid);

                    for (slot, neighbor) in neighbor_list.iter().take(10).enumerate() {
                        if !self.send_to_both(
                            gcs_sysid, bs_sysid,
                            |target_sys, target_comp| {
                                Self::build_neighbor_telemetry(
                                    self_sysid, self_compid,
                                    target_sys, target_comp,
                                    slot,
                                    neighbor.pcid, neighbor.rsrp, neighbor.rssi,
                                    neighbor.rsrq, neighbor.rssnr, neighbor.earfcn,
                                ).expect("slot <= 9")
                            },
                        ).await { return; }
                    }

                    let neighbor_count = neighbor_list.len().min(10);
                    debug!(
                        neighbor_count,
                        "telemetry: sent LTE radio + IP + {} neighbor messages",
                        neighbor_count
                    );
                }
            }
        }
    }

    /// Sends a frame to both GCS and base station targets.
    ///
    /// Builds two copies of the message (one per target) and enqueues them on
    /// the outgoing channel. Returns false if cancelled during send.
    async fn send_to_both(
        &self,
        gcs_sysid: u8,
        bs_sysid: u8,
        build_fn: impl Fn(u8, u8) -> MavFrame,
    ) -> bool {
        let gcs_frame = build_fn(gcs_sysid, 0); // broadcast to all components
        let bs_frame = build_fn(bs_sysid, 0);

        // Check capacity before sending to avoid asymmetric drops where GCS
        // always wins the last slot. If we can't fit both, drop both.
        if self.ctx.tx_outgoing.capacity() < 2 {
            debug!("telemetry: outgoing queue full, dropping GCS+BS messages");
            // Still check if the channel is closed
            if self.ctx.tx_outgoing.is_closed() {
                error!("telemetry: outgoing channel closed");
                return false;
            }
            return true;
        }

        for (label, frame) in [("GCS", gcs_frame), ("BS", bs_frame)] {
            match self.ctx.tx_outgoing.try_send(frame) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // Benign TOCTOU race: another producer took a slot between
                    // our capacity check and this send.
                    debug!("telemetry: outgoing queue full, dropping {label} message");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    error!("telemetry: outgoing channel closed ({label})");
                    return false;
                }
            }
        }
        true
    }

    /// Builds a COMMAND_LONG frame for MAV_CMD_USER_1 with the given subcmd and params.
    ///
    /// The header uses `self_sysid`/`self_compid` as the sender.
    /// `target_system`/`target_component` identify the recipient.
    #[allow(clippy::too_many_arguments)]
    fn build_command_long(
        self_sysid: u8,
        self_compid: u8,
        target_system: u8,
        target_component: u8,
        subcmd: MavCmdUser1SubCmd,
        param2: f32,
        param3: f32,
        param4: f32,
        param5: f32,
        param6: f32,
        param7: f32,
    ) -> MavFrame {
        let header = MavHeader {
            system_id: self_sysid,
            component_id: self_compid,
            sequence: 0,
        };
        let msg = MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
            target_system,
            target_component,
            command: MavCmd::MAV_CMD_USER_1,
            confirmation: 0,
            param1: subcmd.id() as f32,
            param2,
            param3,
            param4,
            param5,
            param6,
            param7,
        });
        (header, msg)
    }

    /// Build an LTE radio telemetry message (subcmd 31014).
    ///
    /// | param | value |
    /// |-------|-------|
    /// | param1 | subcmd ID (31014) |
    /// | param2 | rssi |
    /// | param3 | rsrq |
    /// | param4 | rsrp |
    /// | param5 | rssnr |
    /// | param6 | earfcn |
    /// | param7 | tx_power |
    fn build_lte_radio_telemetry(
        self_sysid: u8,
        self_compid: u8,
        target_system: u8,
        target_component: u8,
        lte: &LteReading,
    ) -> MavFrame {
        Self::build_command_long(
            self_sysid,
            self_compid,
            target_system,
            target_component,
            MavCmdUser1SubCmd::LteRadioTelemetry,
            lte.signal.rssi as f32,
            lte.signal.rsrq as f32,
            lte.signal.rsrp as f32,
            lte.signal.rssnr as f32,
            lte.signal.earfcn as f32,
            lte.signal.tx_power as f32,
        )
    }

    /// Build an LTE IP telemetry message (subcmd 31015).
    ///
    /// | param | value |
    /// |-------|-------|
    /// | param1 | subcmd ID (31015) |
    /// | param2 | is_connected (from ping sensor) |
    /// | param3 | latency_ms (from ping sensor) |
    /// | param4 | loss_percent (from ping sensor) |
    /// | param5 | pcid (from LTE sensor) |
    /// | param6 | neighbor_count |
    /// | param7 | 0 |
    fn build_lte_ip_telemetry(
        self_sysid: u8,
        self_compid: u8,
        target_system: u8,
        target_component: u8,
        ping: &PingReading,
        lte: &LteReading,
    ) -> MavFrame {
        Self::build_command_long(
            self_sysid,
            self_compid,
            target_system,
            target_component,
            MavCmdUser1SubCmd::LteIpTelemetry,
            if ping.reachable { 1.0 } else { 0.0 },
            ping.latency_ms as f32,
            ping.loss_percent as f32,
            lte.signal.pcid as f32,
            lte.neighbors.len() as f32,
            0.0,
        )
    }

    /// Build a neighbor cell telemetry message (subcmds 31040-31049).
    ///
    /// | param | value |
    /// |-------|-------|
    /// | param1 | subcmd ID (31040 + slot) |
    /// | param2 | pcid |
    /// | param3 | rsrp |
    /// | param4 | rssi |
    /// | param5 | rsrq |
    /// | param6 | rssnr |
    /// | param7 | earfcn |
    ///
    /// Returns `None` if the slot index exceeds 9 (max 10 neighbors).
    #[allow(clippy::too_many_arguments)]
    fn build_neighbor_telemetry(
        self_sysid: u8,
        self_compid: u8,
        target_system: u8,
        target_component: u8,
        slot: usize,
        pcid: i32,
        rsrp: i32,
        rssi: i32,
        rsrq: i32,
        rssnr: i32,
        earfcn: i32,
    ) -> Option<MavFrame> {
        let subcmd = match slot {
            0 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors0,
            1 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors1,
            2 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors2,
            3 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors3,
            4 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors4,
            5 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors5,
            6 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors6,
            7 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors7,
            8 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors8,
            9 => MavCmdUser1SubCmd::LteIpTelemetryNeighbors9,
            _ => return None,
        };

        Some(Self::build_command_long(
            self_sysid,
            self_compid,
            target_system,
            target_component,
            subcmd,
            pcid as f32,
            rsrp as f32,
            rssi as f32,
            rsrq as f32,
            rssnr as f32,
            earfcn as f32,
        ))
    }
}

impl Task for TelemetryReporter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let component = Arc::clone(&self);
        let handle = tokio::spawn(async move {
            component.run_loop().await;
        });
        info!("telemetry reporter started");

        vec![handle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::sensors::lte::{LteNeighborCell, LteSignalQuality};
    use std::collections::HashMap;

    fn sample_lte_reading() -> LteReading {
        LteReading {
            signal: LteSignalQuality {
                rsrq: -10,
                rsrp: -85,
                rssi: -60,
                rssnr: 15,
                earfcn: 1300,
                tx_power: 23,
                pcid: 42,
            },
            neighbors: HashMap::new(),
        }
    }

    fn sample_ping_reading(reachable: bool) -> PingReading {
        PingReading {
            reachable,
            latency_ms: if reachable { 25.5 } else { 0.0 },
            loss_percent: if reachable { 0 } else { 100 },
        }
    }

    fn extract_command_long(frame: &MavFrame) -> &COMMAND_LONG_DATA {
        match &frame.1 {
            MavMessage::COMMAND_LONG(data) => data,
            _ => panic!("expected COMMAND_LONG message"),
        }
    }

    // -- LTE radio telemetry tests (subcmd 31014) --

    #[test]
    fn test_lte_radio_telemetry_message_construction() {
        let lte = sample_lte_reading();
        let frame = TelemetryReporter::build_lte_radio_telemetry(1, 10, 255, 190, &lte);

        // Verify header
        assert_eq!(frame.0.system_id, 1);
        assert_eq!(frame.0.component_id, 10);

        // Verify COMMAND_LONG contents
        let data = extract_command_long(&frame);
        assert_eq!(data.target_system, 255);
        assert_eq!(data.target_component, 190);
        assert_eq!(data.command, MavCmd::MAV_CMD_USER_1);
        assert_eq!(data.param1, 31014.0); // LteRadioTelemetry subcmd
        assert_eq!(data.param2, -60.0); // rssi
        assert_eq!(data.param3, -10.0); // rsrq
        assert_eq!(data.param4, -85.0); // rsrp
        assert_eq!(data.param5, 15.0); // rssnr
        assert_eq!(data.param6, 1300.0); // earfcn
        assert_eq!(data.param7, 23.0); // tx_power
    }

    #[test]
    fn test_lte_radio_telemetry_zero_values() {
        let lte = LteReading {
            signal: LteSignalQuality {
                rsrq: 0,
                rsrp: 0,
                rssi: 0,
                rssnr: 0,
                earfcn: 0,
                tx_power: 0,
                pcid: 0,
            },
            neighbors: HashMap::new(),
        };
        let frame = TelemetryReporter::build_lte_radio_telemetry(1, 10, 255, 190, &lte);
        let data = extract_command_long(&frame);
        assert_eq!(data.param2, 0.0);
        assert_eq!(data.param3, 0.0);
        assert_eq!(data.param4, 0.0);
        assert_eq!(data.param5, 0.0);
        assert_eq!(data.param6, 0.0);
        assert_eq!(data.param7, 0.0);
    }

    #[test]
    fn test_lte_radio_telemetry_negative_values() {
        let lte = LteReading {
            signal: LteSignalQuality {
                rsrq: -20,
                rsrp: -140,
                rssi: -110,
                rssnr: -5,
                earfcn: 100,
                tx_power: -10,
                pcid: 1,
            },
            neighbors: HashMap::new(),
        };
        let frame = TelemetryReporter::build_lte_radio_telemetry(1, 10, 200, 0, &lte);
        let data = extract_command_long(&frame);
        assert_eq!(data.param2, -110.0); // rssi
        assert_eq!(data.param3, -20.0); // rsrq
        assert_eq!(data.param4, -140.0); // rsrp
        assert_eq!(data.param5, -5.0); // rssnr
        assert_eq!(data.param7, -10.0); // tx_power
    }

    // -- LTE IP telemetry tests (subcmd 31015) --

    #[test]
    fn test_lte_ip_telemetry_connected() {
        let ping = sample_ping_reading(true);
        let lte = sample_lte_reading();
        let frame = TelemetryReporter::build_lte_ip_telemetry(1, 10, 255, 190, &ping, &lte);

        let data = extract_command_long(&frame);
        assert_eq!(data.param1, 31015.0); // LteIpTelemetry subcmd
        assert_eq!(data.param2, 1.0); // is_connected = true
        assert_eq!(data.param3, 25.5); // latency_ms
        assert_eq!(data.param4, 0.0); // loss_percent
        assert_eq!(data.param5, 42.0); // pcid
        assert_eq!(data.param6, 0.0); // neighbor_count (no neighbors)
        assert_eq!(data.param7, 0.0);
    }

    #[test]
    fn test_lte_ip_telemetry_disconnected() {
        let ping = sample_ping_reading(false);
        let lte = sample_lte_reading();
        let frame = TelemetryReporter::build_lte_ip_telemetry(1, 10, 255, 190, &ping, &lte);

        let data = extract_command_long(&frame);
        assert_eq!(data.param2, 0.0); // is_connected = false
        assert_eq!(data.param3, 0.0); // latency_ms = 0
        assert_eq!(data.param4, 100.0); // loss_percent = 100
    }

    #[test]
    fn test_lte_ip_telemetry_with_neighbors() {
        let ping = sample_ping_reading(true);
        let mut lte = sample_lte_reading();
        // Add 5 neighbors
        for i in 0..5 {
            lte.neighbors.insert(
                100 + i,
                LteNeighborCell {
                    pcid: 100 + i,
                    rsrp: -90,
                    rsrq: -12,
                    rssi: -65,
                    rssnr: 10,
                    earfcn: 1300,
                    last_seen: 0,
                },
            );
        }

        let frame = TelemetryReporter::build_lte_ip_telemetry(1, 10, 255, 190, &ping, &lte);
        let data = extract_command_long(&frame);
        assert_eq!(data.param6, 5.0); // neighbor_count
    }

    #[test]
    fn test_lte_ip_telemetry_partial_loss() {
        let ping = PingReading {
            reachable: true,
            latency_ms: 50.0,
            loss_percent: 30,
        };
        let lte = sample_lte_reading();
        let frame = TelemetryReporter::build_lte_ip_telemetry(1, 10, 255, 190, &ping, &lte);

        let data = extract_command_long(&frame);
        assert_eq!(data.param2, 1.0); // reachable
        assert_eq!(data.param3, 50.0); // latency_ms
        assert_eq!(data.param4, 30.0); // loss_percent
    }

    // -- Neighbor cell telemetry tests (subcmds 31040-31049) --

    #[test]
    fn test_neighbor_telemetry_slot_0() {
        let frame = TelemetryReporter::build_neighbor_telemetry(
            1, 10, 255, 190, 0, 100, -90, -65, -12, 10, 1300,
        )
        .unwrap();

        let data = extract_command_long(&frame);
        assert_eq!(data.param1, 31040.0); // LteIpTelemetryNeighbors0
        assert_eq!(data.param2, 100.0); // pcid
        assert_eq!(data.param3, -90.0); // rsrp
        assert_eq!(data.param4, -65.0); // rssi
        assert_eq!(data.param5, -12.0); // rsrq
        assert_eq!(data.param6, 10.0); // rssnr
        assert_eq!(data.param7, 1300.0); // earfcn
    }

    #[test]
    fn test_neighbor_telemetry_all_slots() {
        for slot in 0..10 {
            let frame = TelemetryReporter::build_neighbor_telemetry(
                1,
                10,
                255,
                190,
                slot,
                slot as i32,
                -90,
                -65,
                -12,
                10,
                0,
            );
            assert!(frame.is_some(), "slot {} should be valid", slot);

            let frame = frame.unwrap();
            let data = extract_command_long(&frame);
            let expected_subcmd = 31040 + slot as u16;
            assert_eq!(
                data.param1, expected_subcmd as f32,
                "wrong subcmd for slot {}",
                slot
            );
            assert_eq!(data.param2, slot as f32, "wrong pcid for slot {}", slot);
        }
    }

    #[test]
    fn test_neighbor_telemetry_slot_out_of_range() {
        let frame = TelemetryReporter::build_neighbor_telemetry(
            1, 10, 255, 190, 10, 100, -90, -65, -12, 10, 1300,
        );
        assert!(frame.is_none());

        let frame = TelemetryReporter::build_neighbor_telemetry(
            1, 10, 255, 190, 100, 100, -90, -65, -12, 10, 1300,
        );
        assert!(frame.is_none());
    }

    #[test]
    fn test_neighbor_telemetry_zero_values() {
        let frame =
            TelemetryReporter::build_neighbor_telemetry(1, 10, 255, 190, 0, 0, 0, 0, 0, 0, 0)
                .unwrap();
        let data = extract_command_long(&frame);
        assert_eq!(data.param2, 0.0);
        assert_eq!(data.param3, 0.0);
        assert_eq!(data.param4, 0.0);
        assert_eq!(data.param5, 0.0);
        assert_eq!(data.param6, 0.0);
        assert_eq!(data.param7, 0.0);
    }

    // -- Integration-style tests --

    fn make_reporter(ctx: Arc<Context>, cancel: CancellationToken) -> Arc<TelemetryReporter> {
        Arc::new(TelemetryReporter::new(ctx, cancel))
    }

    #[tokio::test]
    async fn test_telemetry_run_sends_messages_to_both_targets() {
        let ctx = Context::new(test_config());

        // Set up LTE reading in context
        {
            let mut lte = ctx.sensors.lte.write().await;
            *lte = Some(sample_lte_reading());
        }

        // Set up ping reading
        {
            let mut ping = ctx.sensors.ping.write().await;
            *ping = Some(sample_ping_reading(true));
        }

        let cancel = CancellationToken::new();

        // Take outgoing rx to inspect messages
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        // Wait for messages (first tick is immediate with tokio::time::interval)
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();

        // Collect all messages
        let mut messages = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            messages.push(frame);
        }

        // We should have at minimum 4 messages:
        // 2x LTE radio (GCS + BS) + 2x LTE IP (GCS + BS)
        // (no neighbors in sample_lte_reading)
        assert!(
            messages.len() >= 4,
            "expected at least 4 messages, got {}",
            messages.len()
        );

        // Verify we have messages for both GCS (255) and BS (200) targets
        let gcs_messages: Vec<_> = messages
            .iter()
            .filter(|m| {
                if let MavMessage::COMMAND_LONG(d) = &m.1 {
                    d.target_system == 255
                } else {
                    false
                }
            })
            .collect();

        let bs_messages: Vec<_> = messages
            .iter()
            .filter(|m| {
                if let MavMessage::COMMAND_LONG(d) = &m.1 {
                    d.target_system == 200
                } else {
                    false
                }
            })
            .collect();

        assert!(
            gcs_messages.len() >= 2,
            "expected GCS messages, got {}",
            gcs_messages.len()
        );
        assert!(
            bs_messages.len() >= 2,
            "expected BS messages, got {}",
            bs_messages.len()
        );
    }

    #[tokio::test]
    async fn test_telemetry_run_sends_defaults_without_lte_data() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // No sensor data set — should still send with default (zero) values
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();

        // Should send LTE radio + LTE IP messages (2 per target x 2 targets = 4)
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert!(
            count >= 4 && count % 4 == 0,
            "expected multiple of 4 messages (>= 4), got {}",
            count
        );
    }

    #[tokio::test]
    async fn test_telemetry_run_uses_default_ping_when_missing() {
        let ctx = Context::new(test_config());

        // Set LTE but NOT ping
        {
            let mut lte = ctx.sensors.lte.write().await;
            *lte = Some(sample_lte_reading());
        }

        let cancel = CancellationToken::new();
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();

        // Should still send messages (using default ping = disconnected)
        let mut messages = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            messages.push(frame);
        }

        assert!(
            messages.len() >= 4,
            "should send telemetry with default ping, got {} messages",
            messages.len()
        );

        // Find the LTE IP telemetry message for GCS
        let ip_msg = messages.iter().find(|m| {
            if let MavMessage::COMMAND_LONG(d) = &m.1 {
                d.param1 == 31015.0 && d.target_system == 255
            } else {
                false
            }
        });
        assert!(ip_msg.is_some(), "should have LTE IP telemetry message");

        let data = extract_command_long(ip_msg.unwrap());
        assert_eq!(data.param2, 0.0); // not connected (default)
        assert_eq!(data.param4, 100.0); // 100% loss (default)
    }

    #[tokio::test]
    async fn test_telemetry_run_with_neighbors() {
        let ctx = Context::new(test_config());

        // Set up LTE with 5 neighbors
        {
            let mut lte_reading = sample_lte_reading();
            for i in 0..5 {
                lte_reading.neighbors.insert(
                    200 + i,
                    LteNeighborCell {
                        pcid: 200 + i,
                        rsrp: -90 - i,
                        rsrq: -12,
                        rssi: -65,
                        rssnr: 10,
                        earfcn: 1300,
                        last_seen: 0,
                    },
                );
            }
            let mut lte = ctx.sensors.lte.write().await;
            *lte = Some(lte_reading);
        }

        {
            let mut ping = ctx.sensors.ping.write().await;
            *ping = Some(sample_ping_reading(true));
        }

        let cancel = CancellationToken::new();
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();

        let mut messages = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            messages.push(frame);
        }

        // Expected: 2 radio + 2 IP + 10 neighbor (5 neighbors x 2 targets) = 14
        assert!(
            messages.len() >= 14,
            "expected at least 14 messages (5 neighbors), got {}",
            messages.len()
        );

        // Count neighbor messages (subcmd 31040-31049)
        let neighbor_msgs: Vec<_> = messages
            .iter()
            .filter(|m| {
                if let MavMessage::COMMAND_LONG(d) = &m.1 {
                    d.param1 >= 31040.0 && d.param1 <= 31049.0
                } else {
                    false
                }
            })
            .collect();

        // 5 neighbors x 2 targets = 10 neighbor messages
        assert_eq!(
            neighbor_msgs.len(),
            10,
            "expected 10 neighbor messages, got {}",
            neighbor_msgs.len()
        );
    }

    #[tokio::test]
    async fn test_telemetry_run_caps_neighbors_at_10() {
        let ctx = Context::new(test_config());

        // Set up LTE with 15 neighbors (only first 10 should be sent)
        {
            let mut lte_reading = sample_lte_reading();
            for i in 0..15 {
                lte_reading.neighbors.insert(
                    300 + i,
                    LteNeighborCell {
                        pcid: 300 + i,
                        rsrp: -90,
                        rsrq: -12,
                        rssi: -65,
                        rssnr: 10,
                        earfcn: 1300,
                        last_seen: 0,
                    },
                );
            }
            let mut lte = ctx.sensors.lte.write().await;
            *lte = Some(lte_reading);
        }

        {
            let mut ping = ctx.sensors.ping.write().await;
            *ping = Some(sample_ping_reading(true));
        }

        let cancel = CancellationToken::new();
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        handle.await.unwrap();

        let mut messages = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            messages.push(frame);
        }

        // Count neighbor messages - should be capped at 10 per target = 20
        let neighbor_msgs: Vec<_> = messages
            .iter()
            .filter(|m| {
                if let MavMessage::COMMAND_LONG(d) = &m.1 {
                    d.param1 >= 31040.0 && d.param1 <= 31049.0
                } else {
                    false
                }
            })
            .collect();

        assert_eq!(
            neighbor_msgs.len(),
            20,
            "expected 20 neighbor messages (10 per target), got {}",
            neighbor_msgs.len()
        );
    }

    #[tokio::test]
    async fn test_telemetry_stops_on_cancel() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();
        let _rx = ctx.take_outgoing_rx().await.unwrap();

        let reporter = make_reporter(Arc::clone(&ctx), cancel.clone());
        let handle = tokio::spawn(async move {
            reporter.run_loop().await;
        });

        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("telemetry reporter didn't stop on cancel")
            .unwrap();
    }
}
