//! Message router for fan-out delivery of OMS messages to subscribers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{RwLock, mpsc};

use sleet_types::owp::ServerOp;

type SubscriptionKey = (u64, String);

/// A registered subscription for receiving messages on a topic.
#[derive(Debug)]
pub struct Subscription {
    /// Subscription identifier scoped to the owning connection.
    pub sid: String,
    /// Connection that owns this subscription.
    pub connection_id: u64,
    /// Topic to subscribe to.
    pub topic: String,
    /// Message name (element) to subscribe to.
    pub message_name: String,
    /// Optional queue group for load-balanced delivery.
    pub group: Option<String>,
    /// Channel for delivering messages to the connection task.
    pub sender: mpsc::Sender<ServerOp>,
}

/// State for a single queue group.
#[derive(Debug)]
struct GroupState {
    /// Ordered subset of subscription keys in this group.
    keys: BTreeSet<SubscriptionKey>,
    /// Round-robin delivery index.
    counter: AtomicUsize,
}

impl Default for GroupState {
    fn default() -> Self {
        Self {
            keys: BTreeSet::new(),
            counter: AtomicUsize::new(0),
        }
    }
}

/// Internal mutable state protected by an `RwLock`.
#[derive(Debug)]
struct RouterState {
    /// All active subscriptions keyed by (`connection_id`, SID).
    by_sid: HashMap<SubscriptionKey, Subscription>,
    /// SIDs keyed by `connection_id` for fast limits and cleanup.
    by_connection: HashMap<u64, HashSet<String>>,
    /// Direct (non-group) subscription keys keyed by topic, then `message_name`.
    direct_by_topic: HashMap<String, HashMap<String, HashSet<SubscriptionKey>>>,
    /// Grouped subscription state keyed by topic, `message_name`, then group.
    grouped_by_message: HashMap<String, HashMap<String, HashMap<String, GroupState>>>,
}

struct DeliveryTarget {
    sid: String,
    sender: mpsc::Sender<ServerOp>,
}

/// Shared message router that dispatches published messages to subscribers.
///
/// Subscription identifiers are unique within a connection. PUB fan-out reads
/// the subscription map; SUB/UNSUB write it. Queue groups use round-robin delivery.
#[derive(Debug)]
pub struct Router {
    state: RwLock<RouterState>,
}

/// Errors returned by [`Router`] subscription operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterError {
    /// A subscription with this SID already exists on the connection.
    DuplicateSid(String),
    /// No subscription with this SID was found.
    UnknownSid(String),
    /// The connection has reached the maximum allowed subscriptions.
    TooManySubscriptions,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    /// Create an empty router with no subscriptions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: RwLock::new(RouterState {
                by_sid: HashMap::new(),
                by_connection: HashMap::new(),
                direct_by_topic: HashMap::new(),
                grouped_by_message: HashMap::new(),
            }),
        }
    }

    /// Register a subscription. Returns [`RouterError::DuplicateSid`] if the SID is already used by the connection.
    pub async fn subscribe(&self, sub: Subscription) -> Result<(), RouterError> {
        let mut state = self.state.write().await;

        let connection_subs = state
            .by_connection
            .get(&sub.connection_id)
            .map_or(0, std::collections::HashSet::len);
        if connection_subs >= 2048 {
            return Err(RouterError::TooManySubscriptions);
        }

        let sid = sub.sid.clone();
        let connection_id = sub.connection_id;
        let topic = sub.topic.clone();
        let message_name = sub.message_name.clone();
        let group = sub.group.clone();
        let key = (connection_id, sid.clone());

        if state.by_sid.contains_key(&key) {
            return Err(RouterError::DuplicateSid(sid));
        }

        state.by_sid.insert(key.clone(), sub);
        state
            .by_connection
            .entry(connection_id)
            .or_default()
            .insert(sid);

        if let Some(g) = group {
            state
                .grouped_by_message
                .entry(topic.clone())
                .or_default()
                .entry(message_name)
                .or_default()
                .entry(g)
                .or_default()
                .keys
                .insert(key);
        } else {
            state
                .direct_by_topic
                .entry(topic.clone())
                .or_default()
                .entry(message_name)
                .or_default()
                .insert(key);
        }

        metrics::gauge!("sleet_subscriptions_active", "topic" => topic).increment(1.0);

        drop(state);
        Ok(())
    }

    /// Remove a subscription by SID for a connection.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn unsubscribe(&self, connection_id: u64, sid: &str) -> Result<(), RouterError> {
        let mut state = self.state.write().await;
        let key = (connection_id, sid.to_owned());
        if let Some(removed) = state.by_sid.remove(&key) {
            metrics::gauge!("sleet_subscriptions_active", "topic" => removed.topic.clone())
                .decrement(1.0);

            if let Some(conn_sids) = state.by_connection.get_mut(&removed.connection_id) {
                conn_sids.remove(sid);
                if conn_sids.is_empty() {
                    state.by_connection.remove(&removed.connection_id);
                }
            }

            if let Some(group) = &removed.group {
                remove_group_subscription(
                    &mut state,
                    &removed.topic,
                    &removed.message_name,
                    group,
                    &key,
                );
            } else {
                remove_direct_subscription(&mut state, &removed.topic, &removed.message_name, &key);
            }

            Ok(())
        } else {
            Err(RouterError::UnknownSid(sid.to_owned()))
        }
    }

    /// Fan out a message to all matching subscribers.
    ///
    /// Direct (non-group) subscribers each receive a copy. Within each queue
    /// group, exactly one member receives the message via round-robin.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn publish(&self, topic: &str, message_name: &str, payload: &str) {
        let mut direct_targets: Vec<DeliveryTarget> = Vec::new();
        let mut grouped_targets: Vec<DeliveryTarget> = Vec::new();

        {
            let state = self.state.read().await;

            // Gather direct targets using zero-allocation lookups
            if let Some(topics) = state.direct_by_topic.get(topic)
                && let Some(sids) = topics.get(message_name)
            {
                for sid in sids {
                    if let Some(sub) = state.by_sid.get(sid) {
                        direct_targets.push(DeliveryTarget {
                            sid: sub.sid.clone(),
                            sender: sub.sender.clone(),
                        });
                    }
                }
            }

            // Gather group targets
            if let Some(topics) = state.grouped_by_message.get(topic)
                && let Some(groups_map) = topics.get(message_name)
            {
                for group_state in groups_map.values() {
                    let len = group_state.keys.len();
                    if len > 0 {
                        let current_idx = group_state.counter.fetch_add(1, Ordering::Relaxed);
                        if let Some(selected_key) = group_state.keys.iter().nth(current_idx % len)
                            && let Some(sub) = state.by_sid.get(selected_key)
                        {
                            grouped_targets.push(DeliveryTarget {
                                sid: sub.sid.clone(),
                                sender: sub.sender.clone(),
                            });
                        }
                    }
                }
            }
        }

        if direct_targets.is_empty() && grouped_targets.is_empty() {
            return;
        }

        let payload_arc: std::sync::Arc<str> = std::sync::Arc::from(payload);

        for target in &direct_targets {
            deliver_or_drop(target, &payload_arc);
        }
        for target in &grouped_targets {
            deliver_or_drop(target, &payload_arc);
        }
    }

    /// Remove all subscriptions belonging to a connection (cleanup on disconnect).
    #[allow(clippy::significant_drop_tightening)]
    pub async fn remove_connection(&self, connection_id: u64) {
        let mut state = self.state.write().await;

        if let Some(sids) = state.by_connection.remove(&connection_id) {
            for sid in sids {
                let key = (connection_id, sid);
                if let Some(removed) = state.by_sid.remove(&key) {
                    metrics::gauge!("sleet_subscriptions_active", "topic" => removed.topic.clone())
                        .decrement(1.0);

                    if let Some(group) = removed.group {
                        remove_group_subscription(
                            &mut state,
                            &removed.topic,
                            &removed.message_name,
                            &group,
                            &key,
                        );
                    } else {
                        remove_direct_subscription(
                            &mut state,
                            &removed.topic,
                            &removed.message_name,
                            &key,
                        );
                    }
                }
            }
        }
    }
}

fn remove_direct_subscription(
    state: &mut RouterState,
    topic: &str,
    message_name: &str,
    key: &SubscriptionKey,
) {
    let topic_empty = state.direct_by_topic.get_mut(topic).is_some_and(|topics| {
        let msg_empty = topics.get_mut(message_name).is_some_and(|sids| {
            sids.remove(key);
            sids.is_empty()
        });

        if msg_empty {
            topics.remove(message_name);
        }
        topics.is_empty()
    });

    if topic_empty {
        state.direct_by_topic.remove(topic);
    }
}

fn remove_group_subscription(
    state: &mut RouterState,
    topic: &str,
    message_name: &str,
    group: &str,
    key: &SubscriptionKey,
) {
    let group_empty = state
        .grouped_by_message
        .get_mut(topic)
        .is_some_and(|topics| {
            let msg_empty = topics.get_mut(message_name).is_some_and(|groups| {
                let grp_empty = groups.get_mut(group).is_some_and(|group_state| {
                    group_state.keys.remove(key);
                    group_state.keys.is_empty()
                });

                if grp_empty {
                    groups.remove(group);
                }
                groups.is_empty()
            });

            if msg_empty {
                topics.remove(message_name);
            }
            topics.is_empty()
        });

    if group_empty {
        state.grouped_by_message.remove(topic);
    }
}

fn deliver_or_drop(target: &DeliveryTarget, payload: &std::sync::Arc<str>) {
    let op = ServerOp::Msg {
        sid: target.sid.clone(),
        payload: std::sync::Arc::clone(payload),
    };

    let max_cap = target.sender.max_capacity();
    let current_cap = target.sender.capacity();
    let depth = max_cap.saturating_sub(current_cap);
    #[allow(clippy::cast_precision_loss)]
    metrics::histogram!("sleet_client_queue_depth").record(depth as f64);

    match target.sender.try_send(op) {
        Ok(()) => {
            metrics::counter!("sleet_messages_delivered_total").increment(1);
        }
        Err(TrySendError::Full(_)) => {
            tracing::debug!(sid = %target.sid, "dropping message: subscriber backpressure");
            metrics::counter!(
                "sleet_messages_dropped_total",
                "reason" => "subscriber_backpressure"
            )
            .increment(1);
        }
        Err(TrySendError::Closed(_)) => {
            tracing::debug!(sid = %target.sid, "dropping message: subscriber closed");
            metrics::counter!(
                "sleet_messages_dropped_total",
                "reason" => "subscriber_closed"
            )
            .increment(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn basic_delivery() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "s1".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "AccessAssessment".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();

        router
            .publish("mission", "AccessAssessment", "{\"AccessAssessment\":{}}")
            .await;

        let msg = rx.recv().await.unwrap();
        match msg {
            ServerOp::Msg { sid, payload } => {
                assert_eq!(sid, "s1");
                assert_eq!(&*payload, "{\"AccessAssessment\":{}}");
            }
            other => panic!("expected MSG, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn message_name_filtering() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "s1".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "AccessAssessment".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();

        router.publish("mission", "Other", "{}").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn group_round_robin() {
        let router = Router::new();
        let (tx1, mut rx1) = mpsc::channel(4);
        let (tx2, mut rx2) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "a".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("g1".to_owned()),
                sender: tx1,
            })
            .await
            .unwrap();

        router
            .subscribe(Subscription {
                sid: "b".to_owned(),
                connection_id: 2,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("g1".to_owned()),
                sender: tx2,
            })
            .await
            .unwrap();

        router.publish("mission", "Msg", "{\"n\":1}").await;
        router.publish("mission", "Msg", "{\"n\":2}").await;

        let mut a_count = 0;
        while rx1.try_recv().is_ok() {
            a_count += 1;
        }

        let mut b_count = 0;
        while rx2.try_recv().is_ok() {
            b_count += 1;
        }

        assert_eq!(a_count + b_count, 2);
        assert_eq!(a_count, 1);
        assert_eq!(b_count, 1);
    }

    #[tokio::test]
    async fn group_identity_is_topic_scoped_and_delivery_is_message_filtered() {
        let router = Router::new();
        let (mission_msg_tx, mut mission_msg_rx) = mpsc::channel(4);
        let (mission_other_tx, mut mission_other_rx) = mpsc::channel(4);
        let (other_msg_tx, mut other_msg_rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "mission-msg".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("workers".to_owned()),
                sender: mission_msg_tx,
            })
            .await
            .unwrap();
        router
            .subscribe(Subscription {
                sid: "mission-other".to_owned(),
                connection_id: 2,
                topic: "mission".to_owned(),
                message_name: "Other".to_owned(),
                group: Some("workers".to_owned()),
                sender: mission_other_tx,
            })
            .await
            .unwrap();
        router
            .subscribe(Subscription {
                sid: "other-msg".to_owned(),
                connection_id: 3,
                topic: "other".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("workers".to_owned()),
                sender: other_msg_tx,
            })
            .await
            .unwrap();

        router.publish("mission", "Msg", "{\"n\":1}").await;
        router.publish("mission", "Other", "{\"n\":2}").await;
        router.publish("other", "Msg", "{\"n\":3}").await;

        assert!(matches!(
            mission_msg_rx.recv().await,
            Some(ServerOp::Msg { sid, .. }) if sid == "mission-msg"
        ));
        assert!(matches!(
            mission_other_rx.recv().await,
            Some(ServerOp::Msg { sid, .. }) if sid == "mission-other"
        ));
        assert!(matches!(
            other_msg_rx.recv().await,
            Some(ServerOp::Msg { sid, .. }) if sid == "other-msg"
        ));

        assert!(mission_msg_rx.try_recv().is_err());
        assert!(mission_other_rx.try_recv().is_err());
        assert!(other_msg_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn same_sid_on_different_connections_is_independent() {
        let router = Router::new();
        let (tx1, mut rx1) = mpsc::channel(4);
        let (tx2, mut rx2) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "shared".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx1,
            })
            .await
            .unwrap();
        router
            .subscribe(Subscription {
                sid: "shared".to_owned(),
                connection_id: 2,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx2,
            })
            .await
            .unwrap();

        router.publish("mission", "Msg", "{\"n\":1}").await;
        assert!(matches!(rx1.recv().await, Some(ServerOp::Msg { sid, .. }) if sid == "shared"));
        assert!(matches!(rx2.recv().await, Some(ServerOp::Msg { sid, .. }) if sid == "shared"));

        router.unsubscribe(1, "shared").await.unwrap();
        router.publish("mission", "Msg", "{\"n\":2}").await;

        assert!(rx1.try_recv().is_err());
        assert!(matches!(rx2.recv().await, Some(ServerOp::Msg { sid, .. }) if sid == "shared"));
    }

    #[tokio::test]
    async fn unsubscribe_stops_delivery() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "s1".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();

        router.unsubscribe(1, "s1").await.unwrap();
        router.publish("mission", "Msg", "{}").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unsubscribe_grouped_subscription_stops_delivery() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "grouped".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("workers".to_owned()),
                sender: tx,
            })
            .await
            .unwrap();

        router.unsubscribe(1, "grouped").await.unwrap();
        router.publish("mission", "Msg", "{}").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn connection_cleanup_removes_grouped_subscriptions() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "grouped".to_owned(),
                connection_id: 42,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("workers".to_owned()),
                sender: tx,
            })
            .await
            .unwrap();

        router.remove_connection(42).await;
        router.publish("mission", "Msg", "{}").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn connection_cleanup_removes_subscriptions() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(4);

        router
            .subscribe(Subscription {
                sid: "s1".to_owned(),
                connection_id: 42,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();

        router.remove_connection(42).await;
        router.publish("mission", "Msg", "{}").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn connection_subscription_limit_is_enforced_and_unsubscribe_frees_capacity() {
        let router = Router::new();
        let (tx, _rx) = mpsc::channel(1);

        for n in 0..2048 {
            router
                .subscribe(Subscription {
                    sid: format!("s{n}"),
                    connection_id: 7,
                    topic: "mission".to_owned(),
                    message_name: "Msg".to_owned(),
                    group: None,
                    sender: tx.clone(),
                })
                .await
                .unwrap();
        }

        let result = router
            .subscribe(Subscription {
                sid: "overflow".to_owned(),
                connection_id: 7,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx.clone(),
            })
            .await;
        assert_eq!(result, Err(RouterError::TooManySubscriptions));

        router.unsubscribe(7, "s0").await.unwrap();
        router
            .subscribe(Subscription {
                sid: "replacement".to_owned(),
                connection_id: 7,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn connection_cleanup_frees_subscription_capacity_and_indexes() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(1);

        for n in 0..2048 {
            router
                .subscribe(Subscription {
                    sid: format!("s{n}"),
                    connection_id: 7,
                    topic: "mission".to_owned(),
                    message_name: "Msg".to_owned(),
                    group: None,
                    sender: tx.clone(),
                })
                .await
                .unwrap();
        }

        assert_eq!(
            router
                .subscribe(Subscription {
                    sid: "overflow".to_owned(),
                    connection_id: 7,
                    topic: "mission".to_owned(),
                    message_name: "Msg".to_owned(),
                    group: None,
                    sender: tx.clone(),
                })
                .await,
            Err(RouterError::TooManySubscriptions)
        );

        router.remove_connection(7).await;
        router.publish("mission", "Msg", "{}").await;
        assert!(rx.try_recv().is_err());

        router
            .subscribe(Subscription {
                sid: "replacement".to_owned(),
                connection_id: 7,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: tx,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn saturated_direct_subscriber_drops_message_without_blocking_others() {
        let router = Router::new();
        let (slow_tx, mut slow_rx) = mpsc::channel(1);
        let (fast_tx, mut fast_rx) = mpsc::channel(1);

        slow_tx
            .send(ServerOp::Msg {
                sid: "preexisting".to_owned(),
                payload: std::sync::Arc::from("{}"),
            })
            .await
            .expect("prefill slow subscriber queue");

        router
            .subscribe(Subscription {
                sid: "slow".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: slow_tx,
            })
            .await
            .unwrap();
        router
            .subscribe(Subscription {
                sid: "fast".to_owned(),
                connection_id: 2,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: None,
                sender: fast_tx,
            })
            .await
            .unwrap();

        router.publish("mission", "Msg", "{\"n\":1}").await;

        let fast_msg = fast_rx
            .recv()
            .await
            .expect("fast subscriber receives message");
        match fast_msg {
            ServerOp::Msg { sid, payload } => {
                assert_eq!(sid, "fast");
                assert_eq!(&*payload, "{\"n\":1}");
            }
            other => panic!("expected MSG, got {other:?}"),
        }

        assert!(
            slow_rx.try_recv().is_ok(),
            "preexisting slow message remains"
        );
        assert!(slow_rx.try_recv().is_err(), "new slow message was dropped");
    }

    #[tokio::test]
    async fn saturated_group_subscriber_drops_message_without_blocking_publish() {
        let router = Router::new();
        let (tx, mut rx) = mpsc::channel(1);

        tx.send(ServerOp::Msg {
            sid: "preexisting".to_owned(),
            payload: std::sync::Arc::from("{}"),
        })
        .await
        .expect("prefill group subscriber queue");

        router
            .subscribe(Subscription {
                sid: "grouped".to_owned(),
                connection_id: 1,
                topic: "mission".to_owned(),
                message_name: "Msg".to_owned(),
                group: Some("g1".to_owned()),
                sender: tx,
            })
            .await
            .unwrap();

        router.publish("mission", "Msg", "{\"n\":1}").await;

        assert!(rx.try_recv().is_ok(), "preexisting message remains");
        assert!(
            rx.try_recv().is_err(),
            "saturated group delivery was dropped"
        );
    }
}
