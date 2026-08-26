use std::{collections::HashMap, future::Future, time::Duration};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, Proxy, zvariant::OwnedValue};

use crate::application::{
    NotificationAction, NotificationEvent, NotificationRequest, NotificationSink, PortError,
};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

#[derive(Debug)]
pub struct ZbusNotificationSink {
    connection: Connection,
    supports_actions: bool,
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
        let proxy = notification_proxy(&connection).await?;
        let capabilities: Vec<String> = proxy
            .call("GetCapabilities", &())
            .await
            .map_err(|error| PortError::new("read notification capabilities", error.to_string()))?;
        let listener = subscribe_to_events(connection.clone(), events).await?;

        Ok(Self {
            connection,
            supports_actions: capabilities
                .iter()
                .any(|capability| capability == "actions"),
            timeout_milliseconds: timeout_milliseconds(timeout),
            listener,
        })
    }

    #[must_use]
    pub const fn supports_actions(&self) -> bool {
        self.supports_actions
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
    ) -> impl Future<Output = Result<u32, PortError>> + Send {
        let connection = self.connection.clone();
        let supports_actions = self.supports_actions;
        let timeout = self.timeout_milliseconds;
        async move {
            let proxy = notification_proxy(&connection).await?;
            let actions: &[&str] = if supports_actions && !request.detailed {
                &[
                    "stop",
                    "Остановить",
                    "ignore_hour",
                    "Игнорировать на час",
                    "always_ignore",
                    "Всегда игнорировать",
                    "details",
                    "Подробнее",
                ]
            } else {
                &[]
            };
            let hints = HashMap::<&str, OwnedValue>::new();
            proxy
                .call(
                    "Notify",
                    &(
                        "Resource Guard",
                        0_u32,
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
    let (notification_id, _reason): (u32, u32) = message
        .body()
        .deserialize()
        .map_err(|error| PortError::new("decode notification closure", error.to_string()))?;
    Ok(NotificationEvent::Closed { notification_id })
}

fn timeout_milliseconds(timeout: Duration) -> i32 {
    i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::timeout_milliseconds;

    #[test]
    fn notification_timeout_is_safely_bounded() {
        assert_eq!(timeout_milliseconds(Duration::from_secs(15)), 15_000);
        assert_eq!(timeout_milliseconds(Duration::MAX), i32::MAX);
    }
}
