use std::{collections::HashMap, future::Future, time::Duration};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedValue, Str},
};

use crate::application::{
    NotificationAction, NotificationCloseReason, NotificationEvent, NotificationRequest,
    NotificationSink, NotificationView, PortError,
};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
const DESKTOP_ENTRY: &str = "io.github.denkneb.ResourceGuard";
const SUMMARY_ACTIONS: &[&str] = &[
    "stop",
    "Остановить",
    "ignore_hour",
    "Игнорировать на час",
    "always_ignore",
    "Всегда игнорировать",
    "details",
    "Подробнее",
];
const DETAILS_ACTIONS: &[&str] = &["back", "Назад"];

#[derive(Debug)]
struct NotificationServer {
    name: String,
    vendor: String,
    version: String,
    specification_version: String,
    supports_actions: bool,
    supports_persistence: bool,
}

#[derive(Debug)]
pub struct ZbusNotificationSink {
    connection: Connection,
    server: NotificationServer,
    timeout_milliseconds: i32,
    listener: tokio::task::JoinHandle<()>,
}

impl ZbusNotificationSink {
    /// Connects to the desktop notification service and starts forwarding its signals.
    ///
    /// # Errors
    ///
    /// Returns an error when the session bus, notification proxy, capabilities,
    /// or signal subscriptions cannot be initialized.
    pub async fn connect(
        events: mpsc::Sender<Result<NotificationEvent, PortError>>,
        timeout: Duration,
    ) -> Result<Self, PortError> {
        let connection = Connection::session()
            .await
            .map_err(|error| PortError::new("connect to session D-Bus", error.to_string()))?;
        Self::connect_with_connection(connection, events, timeout).await
    }

    async fn connect_with_connection(
        connection: Connection,
        events: mpsc::Sender<Result<NotificationEvent, PortError>>,
        timeout: Duration,
    ) -> Result<Self, PortError> {
        let proxy = notification_proxy(&connection).await?;
        let capabilities: Vec<String> = proxy
            .call("GetCapabilities", &())
            .await
            .map_err(|error| PortError::new("read notification capabilities", error.to_string()))?;
        let (name, vendor, version, specification_version): (String, String, String, String) =
            proxy
                .call("GetServerInformation", &())
                .await
                .map_err(|error| {
                    PortError::new("read notification server information", error.to_string())
                })?;
        let listener = subscribe_to_events(connection.clone(), events).await?;

        Ok(Self {
            connection,
            server: NotificationServer {
                name,
                vendor,
                version,
                specification_version,
                supports_actions: has_capability(&capabilities, "actions"),
                supports_persistence: has_capability(&capabilities, "persistence"),
            },
            timeout_milliseconds: timeout_milliseconds(timeout),
            listener,
        })
    }

    #[must_use]
    pub const fn supports_actions(&self) -> bool {
        self.server.supports_actions
    }

    #[must_use]
    pub const fn supports_persistence(&self) -> bool {
        self.server.supports_persistence
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server.name
    }

    #[must_use]
    pub fn server_vendor(&self) -> &str {
        &self.server.vendor
    }

    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server.version
    }

    #[must_use]
    pub fn specification_version(&self) -> &str {
        &self.server.specification_version
    }
}

impl Drop for ZbusNotificationSink {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl NotificationSink for ZbusNotificationSink {
    fn notify(
        &mut self,
        request: NotificationRequest,
        replaces_id: Option<u32>,
    ) -> impl Future<Output = Result<u32, PortError>> + Send {
        let connection = self.connection.clone();
        let supports_actions = self.server.supports_actions;
        let supports_persistence = self.server.supports_persistence;
        let timeout = self.timeout_milliseconds;
        async move {
            let proxy = notification_proxy(&connection).await?;
            let actions = notification_actions(request.view, supports_actions);
            let hints = notification_hints(supports_persistence);
            proxy
                .call(
                    "Notify",
                    &(
                        "Resource Guard",
                        replaces_id.unwrap_or_default(),
                        "dialog-warning",
                        request.summary(),
                        request.body(),
                        actions,
                        hints,
                        timeout,
                    ),
                )
                .await
                .map_err(|error| PortError::new("send desktop notification", error.to_string()))
        }
    }

    fn close(
        &mut self,
        notification_id: u32,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        let connection = self.connection.clone();
        async move {
            let proxy = notification_proxy(&connection).await?;
            proxy
                .call("CloseNotification", &(notification_id,))
                .await
                .map_err(|error| PortError::new("close desktop notification", error.to_string()))
        }
    }
}

fn has_capability(capabilities: &[String], expected: &str) -> bool {
    capabilities.iter().any(|capability| capability == expected)
}

fn notification_actions(view: NotificationView, supports_actions: bool) -> &'static [&'static str] {
    if !supports_actions {
        return &[];
    }
    match view {
        NotificationView::Summary => SUMMARY_ACTIONS,
        NotificationView::Details => DETAILS_ACTIONS,
    }
}

fn notification_hints(supports_persistence: bool) -> HashMap<&'static str, OwnedValue> {
    let mut hints = HashMap::from([
        ("desktop-entry", OwnedValue::from(Str::from(DESKTOP_ENTRY))),
        ("transient", OwnedValue::from(false)),
        ("urgency", OwnedValue::from(1_u8)),
    ]);
    if supports_persistence {
        hints.insert("resident", OwnedValue::from(true));
    }
    hints
}

async fn notification_proxy(connection: &Connection) -> Result<Proxy<'_>, PortError> {
    Proxy::new(connection, SERVICE, PATH, INTERFACE)
        .await
        .map_err(|error| PortError::new("create notification proxy", error.to_string()))
}

async fn subscribe_to_events(
    connection: Connection,
    events: mpsc::Sender<Result<NotificationEvent, PortError>>,
) -> Result<tokio::task::JoinHandle<()>, PortError> {
    let proxy = notification_proxy(&connection).await?;
    let action_stream = proxy
        .receive_signal("ActionInvoked")
        .await
        .map_err(|error| PortError::new("subscribe to notification actions", error.to_string()))?;
    let closed_stream = proxy
        .receive_signal("NotificationClosed")
        .await
        .map_err(|error| PortError::new("subscribe to notification closure", error.to_string()))?;

    Ok(tokio::spawn(forward_events(
        connection,
        events,
        action_stream,
        closed_stream,
    )))
}

async fn forward_events(
    _connection: Connection,
    events: mpsc::Sender<Result<NotificationEvent, PortError>>,
    mut actions: zbus::proxy::SignalStream<'static>,
    mut closures: zbus::proxy::SignalStream<'static>,
) {
    loop {
        let event = tokio::select! {
            message = actions.next() => match message {
                Some(message) => parse_action(&message),
                None => Err(PortError::new("listen for notification actions", "signal stream closed")),
            },
            message = closures.next() => match message {
                Some(message) => parse_closed(&message),
                None => Err(PortError::new("listen for notification closure", "signal stream closed")),
            },
        };
        let failed = event.is_err();
        if events.send(event).await.is_err() || failed {
            return;
        }
    }
}

fn parse_action(message: &zbus::Message) -> Result<NotificationEvent, PortError> {
    let (notification_id, key): (u32, String) = message
        .body()
        .deserialize()
        .map_err(|error| PortError::new("decode notification action", error.to_string()))?;
    Ok(NotificationAction::from_key(&key).map_or_else(
        || NotificationEvent::UnknownAction {
            notification_id,
            key,
        },
        |action| NotificationEvent::Action {
            notification_id,
            action,
        },
    ))
}

fn parse_closed(message: &zbus::Message) -> Result<NotificationEvent, PortError> {
    let (notification_id, reason): (u32, u32) = message
        .body()
        .deserialize()
        .map_err(|error| PortError::new("decode notification closure", error.to_string()))?;
    Ok(NotificationEvent::Closed {
        notification_id,
        reason: NotificationCloseReason::from_code(reason),
    })
}

fn timeout_milliseconds(timeout: Duration) -> i32 {
    i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests;
