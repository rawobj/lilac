//! Desktop notifications, for things the user should hear about even with
//! Lilac in the background.

/// Show a notification through the desktop's notification service. Best
/// effort: without one, nothing happens.
pub fn send(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let (summary, body) = (summary.to_string(), body.to_string());
        crate::runtime::spawn(async move {
            let _ = linux::send(&summary, &body).await;
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (summary, body);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use mpris_server::zbus::{self, zvariant::Value};

    pub async fn send(summary: &str, body: &str) -> zbus::Result<()> {
        let connection = zbus::Connection::session().await?;
        let hints: HashMap<&str, Value<'_>> = HashMap::new();
        connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Lilac",
                    0u32,
                    "lilac",
                    summary,
                    body,
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            )
            .await?;
        Ok(())
    }
}
