use async_trait::async_trait;
use modemmanager::dbus::modem::ModemProxy;
use tracing::debug;
use zbus::Connection;

use super::{ModemAccess, ModemError};

/// Real modem accessor using ModemManager D-Bus service.
pub struct DbusModemAccess {
    connection: Connection,
    modem_path: String,
}

impl DbusModemAccess {
    /// Connect to ModemManager and return all available modems.
    ///
    /// Returns one `DbusModemAccess` per modem object path found in
    /// ModemManager. Returns `NoModem` error if no modems are present.
    pub async fn discover_all() -> Result<Vec<Self>, ModemError> {
        let connection = Connection::system()
            .await
            .map_err(|e| ModemError::Dbus(format!("failed to connect to system bus: {}", e)))?;

        // Use ObjectManager to enumerate modems under /org/freedesktop/ModemManager1
        let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
            .destination("org.freedesktop.ModemManager1")
            .map_err(|e| ModemError::Dbus(format!("failed to build proxy: {}", e)))?
            .path("/org/freedesktop/ModemManager1")
            .map_err(|e| ModemError::Dbus(format!("invalid path: {}", e)))?
            .build()
            .await
            .map_err(|e| ModemError::Dbus(format!("failed to create proxy: {}", e)))?;

        let objects = proxy
            .get_managed_objects()
            .await
            .map_err(|e| ModemError::Dbus(format!("failed to enumerate modems: {}", e)))?;

        let modem_paths: Vec<String> = objects
            .keys()
            .filter(|path| path.as_str().contains("/Modem/"))
            .map(|p| p.to_string())
            .collect();

        if modem_paths.is_empty() {
            return Err(ModemError::NoModem);
        }

        let modems = modem_paths
            .into_iter()
            .map(|modem_path| {
                debug!(modem_path = %modem_path, "modem found via D-Bus");
                Self {
                    connection: connection.clone(),
                    modem_path,
                }
            })
            .collect();

        Ok(modems)
    }

    async fn modem_proxy(&self) -> Result<ModemProxy<'_>, ModemError> {
        zbus::proxy::Builder::<'_, ModemProxy<'_>>::new(&self.connection)
            .destination("org.freedesktop.ModemManager1")
            .map_err(|e| ModemError::Dbus(format!("failed to set destination: {}", e)))?
            .path(self.modem_path.as_str())
            .map_err(|e| ModemError::Dbus(format!("invalid modem path: {}", e)))?
            .build()
            .await
            .map_err(|e| ModemError::Dbus(format!("failed to create modem proxy: {}", e)))
    }
}

#[async_trait]
impl ModemAccess for DbusModemAccess {
    async fn model(&self) -> Result<String, ModemError> {
        let proxy = self.modem_proxy().await?;
        proxy
            .model()
            .await
            .map_err(|e| ModemError::Dbus(format!("failed to read model: {}", e)))
    }

    async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError> {
        let proxy = self.modem_proxy().await?;
        proxy.command(cmd, timeout_ms).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("Timeout") || msg.contains("timeout") {
                ModemError::Timeout
            } else {
                ModemError::Dbus(format!("AT command failed: {}", e))
            }
        })
    }
}
