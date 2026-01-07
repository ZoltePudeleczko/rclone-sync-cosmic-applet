use anyhow::Result;

pub fn notify(title: &str, body: &str, is_error: bool) -> Result<()> {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("Rclone Sync Helper");

    if is_error {
        n.urgency(notify_rust::Urgency::Critical);
        n.icon("dialog-error");
    } else {
        n.urgency(notify_rust::Urgency::Normal);
        n.icon("drive-harddisk");
    }

    let _ = n.show()?;
    Ok(())
}
