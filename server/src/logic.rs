use std::collections::HashSet;

use pfboard_shared::{ClientMessage, Point, ServerMessage, Stroke};
use uuid::Uuid;

use crate::state::{Action, Session, TransformSession, MAX_POINTS_PER_STROKE, MAX_STROKES};

pub async fn apply_client_message(
    session: &Session,
    sender: Uuid,
    message: ClientMessage,
) -> Option<(Vec<ServerMessage>, bool)> {
    match message {
        ClientMessage::StrokeStart {
            id,
            color,
            size,
            point,
        } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            let point = normalize_point(point)?;
            let color = sanitize_color(color);
            let size = sanitize_size(size);
            let stroke = Stroke {
                id: id.clone(),
                color: color.clone(),
                size,
                points: vec![point],
            };

            let removed = {
                let mut strokes = session.strokes.write().await;
                strokes.push(stroke);
                let overflow = strokes.len().saturating_sub(MAX_STROKES);
                if overflow > 0 {
                    strokes.drain(0..overflow).collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            };

            if !removed.is_empty() {
                let mut active = session.active_ids.write().await;
                let mut owners = session.owners.write().await;
                for stroke in removed {
                    active.remove(&stroke.id);
                    owners.remove(&stroke.id);
                }
            }

            session.active_ids.write().await.insert(id.clone());
            session.owners.write().await.insert(id.clone(), sender);
            session.mark_dirty();

            Some((
                vec![ServerMessage::StrokeStart {
                    id,
                    color,
                    size,
                    point,
                }],
                false,
            ))
        }
        ClientMessage::StrokeMove { id, point } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            let point = normalize_point(point)?;
            if !session.active_ids.read().await.contains(&id) {
                return None;
            }

            let mut appended = false;
            {
                let mut strokes = session.strokes.write().await;
                if let Some(stroke) = strokes.iter_mut().find(|stroke| stroke.id == id) {
                    if stroke.points.len() < MAX_POINTS_PER_STROKE {
                        stroke.points.push(point);
                        appended = true;
                    }
                }
            }

            if appended {
                session.mark_dirty();
                Some((vec![ServerMessage::StrokeMove { id, point }], false))
            } else {
                None
            }
        }
        ClientMessage::StrokeEnd { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            session.active_ids.write().await.remove(&id);
            if let Some(owner) = session.owners.read().await.get(&id) {
                if *owner == sender {
                    let stroke = {
                        let strokes = session.strokes.read().await;
                        strokes.iter().find(|stroke| stroke.id == id).cloned()
                    };
                    if let Some(stroke) = stroke {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::AddStroke(stroke));
                            history.redo.clear();
                        }
                    }
                }
            }
            Some((vec![ServerMessage::StrokeEnd { id }], false))
        }
        ClientMessage::Clear => {
            let cleared = session.strokes.write().await.drain(..).collect::<Vec<_>>();
            session.active_ids.write().await.clear();
            session.owners.write().await.clear();
            session.transform_sessions.write().await.clear();
            session.mark_dirty();
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                history.undo.push(Action::Clear { strokes: cleared });
                history.redo.clear();
            }
            Some((vec![ServerMessage::Clear], false))
        }
        ClientMessage::Undo => {
            let action = {
                let mut histories = session.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.undo.pop())
            }?;

            match action {
                Action::AddStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id).await {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.redo.push(Action::AddStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::EraseStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender)).await;
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.redo.push(Action::EraseStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::Clear { strokes } => {
                    for stroke in &strokes {
                        add_stroke(session, stroke.clone(), None).await;
                    }
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.redo.push(Action::Clear {
                            strokes: strokes.clone(),
                        });
                    }
                    let messages = strokes
                        .into_iter()
                        .map(|stroke| ServerMessage::StrokeRestore { stroke })
                        .collect::<Vec<_>>();
                    Some((messages, true))
                }
                Action::ReplaceStroke { before, after } => {
                    let replaced = replace_stroke(session, before.clone()).await;
                    if replaced.is_some() {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.redo.push(Action::ReplaceStroke {
                                before: before.clone(),
                                after,
                            });
                        }
                        Some((vec![ServerMessage::StrokeReplace { stroke: before }], true))
                    } else {
                        None
                    }
                }
                Action::Transform { before, after } => {
                    let mut replaced = Vec::new();
                    for stroke in &before {
                        if replace_stroke(session, stroke.clone()).await.is_some() {
                            replaced.push(stroke.clone());
                        }
                    }
                    if replaced.is_empty() {
                        return None;
                    }
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.redo.push(Action::Transform {
                            before: before.clone(),
                            after,
                        });
                    }
                    let messages = replaced
                        .into_iter()
                        .map(|stroke| ServerMessage::StrokeReplace { stroke })
                        .collect::<Vec<_>>();
                    Some((messages, true))
                }
            }
        }
        ClientMessage::Redo => {
            let action = {
                let mut histories = session.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.redo.pop())
            }?;

            match action {
                Action::AddStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender)).await;
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.undo.push(Action::AddStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::EraseStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id).await {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::EraseStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::Clear { strokes } => {
                    session.strokes.write().await.clear();
                    session.active_ids.write().await.clear();
                    session.owners.write().await.clear();
                    session.mark_dirty();
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.undo.push(Action::Clear { strokes });
                    }
                    Some((vec![ServerMessage::Clear], true))
                }
                Action::ReplaceStroke { before, after } => {
                    let replaced = replace_stroke(session, after.clone()).await;
                    if replaced.is_some() {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::ReplaceStroke {
                                before,
                                after: after.clone(),
                            });
                        }
                        Some((vec![ServerMessage::StrokeReplace { stroke: after }], true))
                    } else {
                        None
                    }
                }
                Action::Transform { before, after } => {
                    let mut replaced = Vec::new();
                    for stroke in &after {
                        if replace_stroke(session, stroke.clone()).await.is_some() {
                            replaced.push(stroke.clone());
                        }
                    }
                    if replaced.is_empty() {
                        return None;
                    }
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.undo.push(Action::Transform {
                            before,
                            after: after.clone(),
                        });
                    }
                    let messages = replaced
                        .into_iter()
                        .map(|stroke| ServerMessage::StrokeReplace { stroke })
                        .collect::<Vec<_>>();
                    Some((messages, true))
                }
            }
        }
        ClientMessage::Erase { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }

            let removed = {
                let mut strokes = session.strokes.write().await;
                if let Some(index) = strokes.iter().position(|s| s.id == id) {
                    Some(strokes.remove(index))
                } else {
                    None
                }
            };

            if let Some(stroke) = removed {
                session.active_ids.write().await.remove(&id);
                session.owners.write().await.remove(&id);
                let mut histories = session.histories.write().await;
                if let Some(history) = histories.get_mut(&sender) {
                    history.undo.push(Action::EraseStroke(stroke));
                    history.redo.clear();
                }
                session.mark_dirty();
                Some((vec![ServerMessage::StrokeRemove { id }], true))
            } else {
                None
            }
        }
        ClientMessage::StrokeReplace { stroke } => {
            let stroke = sanitize_stroke(stroke)?;
            let before = replace_stroke(session, stroke.clone()).await?;
            let in_transform = session
                .transform_sessions
                .read()
                .await
                .contains_key(&sender);
            if !in_transform {
                let mut histories = session.histories.write().await;
                if let Some(history) = histories.get_mut(&sender) {
                    history
                        .undo
                        .push(Action::ReplaceStroke { before, after: stroke.clone() });
                    history.redo.clear();
                }
            }
            Some((vec![ServerMessage::StrokeReplace { stroke }], false))
        }
        ClientMessage::TransformStart { ids } => {
            let ids = sanitize_ids(ids);
            if ids.is_empty() {
                return None;
            }
            let before = {
                let strokes = session.strokes.read().await;
                strokes
                    .iter()
                    .filter(|stroke| ids.iter().any(|id| id == &stroke.id))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            session.transform_sessions.write().await.insert(
                sender,
                TransformSession {
                    ids,
                    before,
                },
            );
            None
        }
        ClientMessage::TransformEnd { ids: _ } => {
            let session_info = session.transform_sessions.write().await.remove(&sender);
            let Some(session_info) = session_info else {
                return None;
            };
            let after = {
                let strokes = session.strokes.read().await;
                strokes
                    .iter()
                    .filter(|stroke| session_info.ids.iter().any(|id| id == &stroke.id))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if session_info.before.is_empty() || after.is_empty() {
                return None;
            }
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                history.undo.push(Action::Transform {
                    before: session_info.before,
                    after,
                });
                history.redo.clear();
            }
            None
        }
        ClientMessage::Remove { ids } => {
            if ids.is_empty() {
                return None;
            }
            let mut removed = Vec::new();
            for id in ids {
                if id.is_empty() || id.len() > 64 {
                    continue;
                }
                let stroke = remove_stroke_full(session, &id).await;
                if let Some(stroke) = stroke {
                    removed.push(stroke);
                }
            }
            if removed.is_empty() {
                return None;
            }
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                for stroke in &removed {
                    history.undo.push(Action::EraseStroke(stroke.clone()));
                }
                history.redo.clear();
            }
            let messages = removed
                .into_iter()
                .map(|stroke| ServerMessage::StrokeRemove { id: stroke.id })
                .collect::<Vec<_>>();
            Some((messages, false))
        }
        ClientMessage::Load { strokes } => {
            let strokes = sanitize_strokes(strokes);
            {
                let mut stored = session.strokes.write().await;
                *stored = strokes.clone();
            }
            session.active_ids.write().await.clear();
            session.owners.write().await.clear();
            session.transform_sessions.write().await.clear();
            session.mark_dirty();
            let mut histories = session.histories.write().await;
            for history in histories.values_mut() {
                history.undo.clear();
                history.redo.clear();
            }
            Some((vec![ServerMessage::Sync { strokes }], true))
        }
    }
}

pub async fn broadcast_except(session: &Session, sender: Uuid, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = session.peers.read().await;
        for (id, tx) in peers.iter() {
            if *id == sender {
                continue;
            }
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut peers = session.peers.write().await;
        for id in stale {
            peers.remove(&id);
        }
    }
}

pub async fn broadcast_all(session: &Session, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = session.peers.read().await;
        for (id, tx) in peers.iter() {
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut peers = session.peers.write().await;
        for id in stale {
            peers.remove(&id);
        }
    }
}

pub fn sanitize_strokes(strokes: Vec<Stroke>) -> Vec<Stroke> {
    strokes.into_iter().filter_map(sanitize_stroke).collect()
}

fn normalize_point(point: Point) -> Option<Point> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    Some(point)
}

fn sanitize_color(mut color: String) -> String {
    if color.is_empty() {
        return "#1f1f1f".to_string();
    }
    if color.len() > 32 {
        color.truncate(32);
    }
    color
}

fn sanitize_size(size: f32) -> f32 {
    let size = if size.is_finite() { size } else { 6.0 };
    size.max(1.0).min(60.0)
}

fn sanitize_stroke(mut stroke: Stroke) -> Option<Stroke> {
    if stroke.id.is_empty() || stroke.id.len() > 64 {
        return None;
    }
    stroke.color = sanitize_color(stroke.color);
    stroke.size = sanitize_size(stroke.size);
    stroke.points = stroke
        .points
        .into_iter()
        .filter_map(normalize_point)
        .collect();
    if stroke.points.is_empty() {
        return None;
    }
    Some(stroke)
}

fn sanitize_ids(ids: Vec<String>) -> Vec<String> {
    let mut unique = HashSet::new();
    let mut result = Vec::new();
    for id in ids {
        if id.is_empty() || id.len() > 64 {
            continue;
        }
        if unique.insert(id.clone()) {
            result.push(id);
        }
    }
    result
}

async fn remove_stroke(session: &Session, id: &str) -> bool {
    let removed = {
        let mut strokes = session.strokes.write().await;
        if let Some(index) = strokes.iter().position(|s| s.id == id) {
            strokes.remove(index);
            true
        } else {
            false
        }
    };
    if removed {
        session.active_ids.write().await.remove(id);
        session.owners.write().await.remove(id);
        session.mark_dirty();
    }
    removed
}

async fn add_stroke(session: &Session, stroke: Stroke, owner: Option<Uuid>) {
    let removed = {
        let mut strokes = session.strokes.write().await;
        strokes.push(stroke.clone());
        let overflow = strokes.len().saturating_sub(MAX_STROKES);
        if overflow > 0 {
            strokes.drain(0..overflow).collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };

    if !removed.is_empty() {
        let mut active = session.active_ids.write().await;
        let mut owners = session.owners.write().await;
        for stroke in removed {
            active.remove(&stroke.id);
            owners.remove(&stroke.id);
        }
    }

    if let Some(owner) = owner {
        session.owners.write().await.insert(stroke.id.clone(), owner);
    }
    session.mark_dirty();
}

async fn replace_stroke(session: &Session, stroke: Stroke) -> Option<Stroke> {
    let mut strokes = session.strokes.write().await;
    if let Some(index) = strokes.iter().position(|s| s.id == stroke.id) {
        let before = strokes[index].clone();
        strokes[index] = stroke;
        session.mark_dirty();
        Some(before)
    } else {
        None
    }
}

async fn remove_stroke_full(session: &Session, id: &str) -> Option<Stroke> {
    let removed = {
        let mut strokes = session.strokes.write().await;
        if let Some(index) = strokes.iter().position(|s| s.id == id) {
            Some(strokes.remove(index))
        } else {
            None
        }
    };
    if removed.is_some() {
        session.active_ids.write().await.remove(id);
        session.owners.write().await.remove(id);
        session.mark_dirty();
    }
    removed
}
