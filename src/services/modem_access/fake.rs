use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use super::{ModemAccess, ModemError};

/// Fixed IMSI value returned by the fake modem.
const FAKE_IMSI: &str = "001010123456789";

/// Deterministic ModemAccess implementation for the simulation image.
///
/// Drives a monotonic counter so signal-quality fields drift smoothly
/// across calls. No randomness — tests are repeatable.
pub struct FakeModemAccess {
    counter: AtomicU64,
}

impl FakeModemAccess {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }
}

impl Default for FakeModemAccess {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModemAccess for FakeModemAccess {
    async fn model(&self) -> Result<String, ModemError> {
        Ok("EM12".to_string())
    }

    async fn command(&self, cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
        let cmd = cmd.trim();
        if cmd.starts_with("AT+QENG=\"neighbourcell\"") {
            return Ok(self.synth_neighbourcell());
        }
        if cmd.starts_with("AT+QENG=\"servingcell\"") {
            return Ok(self.synth_servingcell());
        }
        if cmd.starts_with("AT+CIMI") {
            return Ok(format!("{FAKE_IMSI}\r\nOK"));
        }
        if cmd.starts_with("AT+CEREG?") {
            return Ok("+CEREG: 0,1\r\nOK".to_string());
        }
        if cmd.starts_with("AT+CREG?") {
            return Ok("+CREG: 0,1\r\nOK".to_string());
        }
        Ok("OK".to_string())
    }

    async fn imsi(&self) -> Result<String, ModemError> {
        Ok(FAKE_IMSI.to_string())
    }
}

impl FakeModemAccess {
    fn next_counter(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn synth_servingcell(&self) -> String {
        let n = self.next_counter();
        // Drift each metric inside its valid LTE band using cheap modular arithmetic.
        let pcid = 250i32;
        let earfcn = 1850i32;
        let rsrp = -90 + ((n % 21) as i32 - 10); // -100..-80
        let rsrq = -10 + ((n % 7) as i32 - 3); // -13..-7
        let rssi = -65 + ((n % 11) as i32 - 5); // -70..-60
        let rssnr = 5 + ((n % 11) as i32 - 5); // 0..10
        let tx_power = -10 + ((n % 5) as i32); // -10..-6

        // 20 comma-separated fields per parse_quectel_em12_serving:
        // 0:"+QENG: \"servingcell\"" 1:"NOCONN" 2:"LTE" 3:"FDD" 4:mcc 5:mnc
        // 6:cell_id 7:pcid 8:earfcn 9:freq_band 10:ul_bw 11:dl_bw 12:tac
        // 13:rsrp 14:rsrq 15:rssi 16:rssnr 17:cqi 18:tx_power 19:srxlev
        format!(
            "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",001,01,\"1A2B3C4D\",{pcid},{earfcn},3,5,5,\"1234\",{rsrp},{rsrq},{rssi},{rssnr},10,{tx_power},42\r\nOK"
        )
    }

    fn synth_neighbourcell(&self) -> String {
        let n = self.next_counter();
        let earfcn = 1850i32;

        // Two cells with distinct pcids that drift independently.
        let cells = [
            (
                100i32,         // pcid
                -12 + ((n % 5) as i32 - 2),    // rsrq
                -100 + ((n % 21) as i32 - 10), // rsrp
                -75 + ((n % 11) as i32 - 5),   // rssi
                3 + ((n % 7) as i32 - 3),      // rssnr
            ),
            (
                200i32,
                -14 + ((n % 5) as i32 - 2),
                -105 + ((n % 21) as i32 - 10),
                -80 + ((n % 11) as i32 - 5),
                1 + ((n % 7) as i32 - 3),
            ),
        ];

        // Format per parse_quectel_neighbor:
        //   0:"+QENG: \"neighbourcell intra\"" 1:mode 2:earfcn 3:pcid 4:rsrq
        //   5:rsrp 6:rssi 7:rssnr [8:..]
        let mut out = String::new();
        for (pcid, rsrq, rsrp, rssi, rssnr) in cells {
            out.push_str(&format!(
                "+QENG: \"neighbourcell intra\",\"LTE\",{earfcn},{pcid},{rsrq},{rsrp},{rssi},{rssnr},5,8,-,-\r\n"
            ));
        }
        out.push_str("OK");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_model_returns_em12() {
        let modem = FakeModemAccess::new();
        assert_eq!(modem.model().await.unwrap(), "EM12");
    }

    #[tokio::test]
    async fn test_unknown_command_returns_ok() {
        let modem = FakeModemAccess::new();
        assert_eq!(modem.command("AT+UNKNOWN", 1000).await.unwrap(), "OK");
    }

    #[tokio::test]
    async fn test_imsi_returns_fixed_value() {
        let modem = FakeModemAccess::new();
        assert_eq!(modem.imsi().await.unwrap(), "001010123456789");
    }

    #[tokio::test]
    async fn test_cimi_command_returns_imsi_payload() {
        let modem = FakeModemAccess::new();
        let resp = modem.command("AT+CIMI", 1000).await.unwrap();
        assert!(
            resp.contains("001010123456789"),
            "expected IMSI in response, got {:?}",
            resp
        );
    }

    #[tokio::test]
    async fn test_cereg_returns_registered_home() {
        let modem = FakeModemAccess::new();
        let resp = modem.command("AT+CEREG?", 1000).await.unwrap();
        assert!(resp.contains("+CEREG: 0,1"), "got {:?}", resp);
    }

    #[tokio::test]
    async fn test_creg_returns_registered_home() {
        let modem = FakeModemAccess::new();
        let resp = modem.command("AT+CREG?", 1000).await.unwrap();
        assert!(resp.contains("+CREG: 0,1"), "got {:?}", resp);
    }

    #[tokio::test]
    async fn test_registration_status_round_trips() {
        use super::super::NetworkRegistration;
        let modem = FakeModemAccess::new();
        assert_eq!(
            modem.registration_status().await.unwrap(),
            NetworkRegistration::RegisteredHome
        );
    }

    #[tokio::test]
    async fn test_servingcell_round_trips_through_em12_parser() {
        use crate::sensors::lte::parse_quectel_em12_serving;

        let modem = FakeModemAccess::new();
        let resp = modem
            .command("AT+QENG=\"servingcell\"", 1000)
            .await
            .unwrap();

        let signal =
            parse_quectel_em12_serving(&resp).expect("synthesized servingcell response must parse");

        // Sanity: counter starts at 0, drift offsets are well within valid LTE ranges.
        assert!(
            signal.rsrp <= -60 && signal.rsrp >= -140,
            "rsrp={}",
            signal.rsrp
        );
        assert!(
            signal.rsrq <= -3 && signal.rsrq >= -20,
            "rsrq={}",
            signal.rsrq
        );
        assert!(
            signal.rssi <= -40 && signal.rssi >= -110,
            "rssi={}",
            signal.rssi
        );
        assert!(
            signal.rssnr >= -10 && signal.rssnr <= 30,
            "rssnr={}",
            signal.rssnr
        );
        assert!(
            signal.pcid >= 250 && signal.pcid < 504,
            "pcid={}",
            signal.pcid
        );
        assert!(signal.earfcn > 0, "earfcn={}", signal.earfcn);
    }

    #[tokio::test]
    async fn test_servingcell_values_drift_between_calls() {
        use crate::sensors::lte::parse_quectel_em12_serving;

        let modem = FakeModemAccess::new();
        let r1 = parse_quectel_em12_serving(
            &modem
                .command("AT+QENG=\"servingcell\"", 1000)
                .await
                .unwrap(),
        )
        .unwrap();
        let r2 = parse_quectel_em12_serving(
            &modem
                .command("AT+QENG=\"servingcell\"", 1000)
                .await
                .unwrap(),
        )
        .unwrap();
        // At least one numeric field must differ — otherwise nothing is drifting.
        assert!(
            r1.rsrp != r2.rsrp || r1.rsrq != r2.rsrq || r1.rssnr != r2.rssnr,
            "values should drift between calls: {:?} vs {:?}",
            r1,
            r2
        );
    }

    #[tokio::test]
    async fn test_neighbourcell_returns_two_distinct_cells() {
        use crate::sensors::lte::parse_quectel_neighbor;

        let modem = FakeModemAccess::new();
        let resp = modem
            .command("AT+QENG=\"neighbourcell\"", 1000)
            .await
            .unwrap();

        let cells: Vec<_> = resp.lines().filter_map(parse_quectel_neighbor).collect();
        assert_eq!(cells.len(), 2, "got {:#?} from response {:?}", cells, resp);
        assert_ne!(
            cells[0].pcid, cells[1].pcid,
            "neighbours must have distinct pcids"
        );
    }
}
