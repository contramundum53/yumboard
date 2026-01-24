use std::collections::HashSet;
use std::sync::Arc;

use pfboard_shared::{ClientMessage, Point, ServerMessage, Stroke};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::state::{Action, Session, TransformSession, MAX_POINTS_PER_STROKE, MAX_STROKES};

pub fn apply_client_message(
    session: &mut Session,
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

            session.strokes.push(stroke);
            let overflow = session.strokes.len().saturating_sub(MAX_STROKES);
            if overflow > 0 {
                let removed = session.strokes.drain(0..overflow).collect::<Vec<_>>();
                for stroke in removed {
                    session.active_ids.remove(&stroke.id);
                    session.owners.remove(&stroke.id);
                }
            }
            session.active_ids.insert(id.clone());
            session.owners.insert(id.clone(), sender);
            session.dirty = true;

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
            if !session.active_ids.contains(&id) {
                return None;
            }
            if let Some(stroke) = session.strokes.iter_mut().find(|stroke| stroke.id == id) {
                if stroke.points.len() < MAX_POINTS_PER_STROKE {
                    stroke.points.push(point);
                    session.dirty = true;

                    return Some((vec![ServerMessage::StrokeMove { id, point }], false));
                }
            }
            None
        }
        ClientMessage::StrokeEnd { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            session.active_ids.remove(&id);
            if let Some(owner) = session.owners.get(&id) {
                if *owner == sender {
                    let stroke = session
                        .strokes
                        .iter()
                        .find(|stroke| stroke.id == id)
                        .cloned();
                    if let Some(stroke) = stroke {
                        if let Some(history) = session.histories.get_mut(&sender) {
                            history.undo.push(Action::AddStroke(stroke));
                            history.redo.clear();
                        }
                    }
                }
            }
            Some((vec![ServerMessage::StrokeEnd { id }], false))
        }
        ClientMessage::Clear => {
            let cleared = session.strokes.drain(..).collect::<Vec<_>>();
            session.active_ids.clear();
            session.owners.clear();
            session.transform_sessions.clear();
            session.dirty = true;

            if let Some(history) = session.histories.get_mut(&sender) {
                history.undo.push(Action::Clear { strokes: cleared });
                history.redo.clear();
            }
            Some((vec![ServerMessage::Clear], false))
        }
        ClientMessage::Undo => {
            let action = session
                .histories
                .get_mut(&sender)
                .and_then(|history| history.undo.pop())?;

            match action {
                Action::AddStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id) {
                        if let Some(history) = session.histories.get_mut(&sender) {
                            history.redo.push(Action::AddStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::EraseStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender));
                    if let Some(history) = session.histories.get_mut(&sender) {
                        history.redo.push(Action::EraseStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::Clear { strokes } => {
                    for stroke in &strokes {
                        add_stroke(session, stroke.clone(), None);
                    }
                    if let Some(history) = session.histories.get_mut(&sender) {
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
                    let replaced = replace_stroke(session, before.clone());
                    if replaced.is_some() {
                        if let Some(history) = session.histories.get_mut(&sender) {
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
                        if replace_stroke(session, stroke.clone()).is_some() {
                            replaced.push(stroke.clone());
                        }
                    }
                    if replaced.is_empty() {
                        return None;
                    }
                    if let Some(history) = session.histories.get_mut(&sender) {
                        history.redo.push(Action::Transform { before, after });
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
            let action = session
                .histories
                .get_mut(&sender)
                .and_then(|history| history.redo.pop())?;

            match action {
                Action::AddStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender));
                    if let Some(history) = session.histories.get_mut(&sender) {
                        history.undo.push(Action::AddStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::EraseStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id) {
                        if let Some(history) = session.histories.get_mut(&sender) {
                            history.undo.push(Action::EraseStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::Clear { strokes } => {
                    session.strokes.clear();
                    session.active_ids.clear();
                    session.owners.clear();
                    session.dirty = true;

                    if let Some(history) = session.histories.get_mut(&sender) {
                        history.undo.push(Action::Clear { strokes });
                    }
                    Some((vec![ServerMessage::Clear], true))
                }
                Action::ReplaceStroke { before, after } => {
                    let replaced = replace_stroke(session, after.clone());
                    if replaced.is_some() {
                        if let Some(history) = session.histories.get_mut(&sender) {
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
                        if replace_stroke(session, stroke.clone()).is_some() {
                            replaced.push(stroke.clone());
                        }
                    }
                    if replaced.is_empty() {
                        return None;
                    }
                    if let Some(history) = session.histories.get_mut(&sender) {
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

            let removed = if let Some(index) = session.strokes.iter().position(|s| s.id == id) {
                Some(session.strokes.remove(index))
            } else {
                None
            };

            if let Some(stroke) = removed {
                session.active_ids.remove(&id);
                session.owners.remove(&id);
                if let Some(history) = session.histories.get_mut(&sender) {
                    history.undo.push(Action::EraseStroke(stroke));
                    history.redo.clear();
                }
                session.dirty = true;

                Some((vec![ServerMessage::StrokeRemove { id }], true))
            } else {
                None
            }
        }
        ClientMessage::StrokeReplace { stroke } => {
            let stroke = sanitize_stroke(stroke)?;
            let before = replace_stroke(session, stroke.clone())?;
            let in_transform = session.transform_sessions.contains_key(&sender);
            if !in_transform {
                if let Some(history) = session.histories.get_mut(&sender) {
                    history.undo.push(Action::ReplaceStroke {
                        before,
                        after: stroke.clone(),
                    });
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
            let before = session
                .strokes
                .iter()
                .filter(|stroke| ids.iter().any(|id| id == &stroke.id))
                .cloned()
                .collect::<Vec<_>>();
            session
                .transform_sessions
                .insert(sender, TransformSession { ids, before });
            None
        }
        ClientMessage::TransformEnd { ids: _ } => {
            let session_info = session.transform_sessions.remove(&sender);
            let Some(session_info) = session_info else {
                return None;
            };
            let after = session
                .strokes
                .iter()
                .filter(|stroke| session_info.ids.iter().any(|id| id == &stroke.id))
                .cloned()
                .collect::<Vec<_>>();
            if session_info.before.is_empty() || after.is_empty() {
                return None;
            }
            if let Some(history) = session.histories.get_mut(&sender) {
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
                let stroke = remove_stroke_full(session, &id);
                if let Some(stroke) = stroke {
                    removed.push(stroke);
                }
            }
            if removed.is_empty() {
                return None;
            }
            if let Some(history) = session.histories.get_mut(&sender) {
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
            session.strokes = strokes.clone();
            session.active_ids.clear();
            session.owners.clear();
            session.transform_sessions.clear();
            session.dirty = true;

            for history in session.histories.values_mut() {
                history.undo.clear();
                history.redo.clear();
            }
            Some((vec![ServerMessage::Sync { strokes }], true))
        }
    }
}

pub async fn broadcast_except(
    session: &Arc<RwLock<Session>>,
    sender: Uuid,
    message: ServerMessage,
) {
    let mut stale = Vec::new();
    {
        let session = session.read().await;
        for (id, tx) in session.peers.iter() {
            if *id == sender {
                continue;
            }
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut session = session.write().await;
        for id in stale {
            session.peers.remove(&id);
        }
    }
}

pub async fn broadcast_all(session: &Arc<RwLock<Session>>, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let session = session.read().await;
        for (id, tx) in session.peers.iter() {
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut session = session.write().await;
        for id in stale {
            session.peers.remove(&id);
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

fn remove_stroke(session: &mut Session, id: &str) -> bool {
    let removed = if let Some(index) = session.strokes.iter().position(|s| s.id == id) {
        session.strokes.remove(index);
        true
    } else {
        false
    };
    if removed {
        session.active_ids.remove(id);
        session.owners.remove(id);
        session.dirty = true;
    }
    removed
}

fn add_stroke(session: &mut Session, stroke: Stroke, owner: Option<Uuid>) {
    session.strokes.push(stroke.clone());
    let overflow = session.strokes.len().saturating_sub(MAX_STROKES);
    if overflow > 0 {
        let removed = session.strokes.drain(0..overflow).collect::<Vec<_>>();
        for stroke in removed {
            session.active_ids.remove(&stroke.id);
            session.owners.remove(&stroke.id);
        }
    }

    if let Some(owner) = owner {
        session.owners.insert(stroke.id.clone(), owner);
    }
    session.dirty = true;
}

fn replace_stroke(session: &mut Session, stroke: Stroke) -> Option<Stroke> {
    if let Some(index) = session.strokes.iter().position(|s| s.id == stroke.id) {
        let before = session.strokes[index].clone();
        session.strokes[index] = stroke;
        session.dirty = true;

        Some(before)
    } else {
        None
    }
}

fn remove_stroke_full(session: &mut Session, id: &str) -> Option<Stroke> {
    let removed = if let Some(index) = session.strokes.iter().position(|s| s.id == id) {
        Some(session.strokes.remove(index))
    } else {
        None
    };
    if removed.is_some() {
        session.active_ids.remove(id);
        session.owners.remove(id);
        session.dirty = true;
    }
    removed
}
