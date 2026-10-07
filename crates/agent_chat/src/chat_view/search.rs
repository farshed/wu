use super::ChatView;
use crate::session::Entry;
use gpui::{App, AppContext as _, Context, Entity, Focusable as _, ListOffset, Task, Window};
use markdown::Markdown;
use project::search::SearchQuery;
use std::{ops::Range, sync::Arc};
use workspace::searchable::{Direction, SearchOptions, SearchToken, SearchableItem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMatch {
    entry: usize,
    range: Range<usize>,
}

impl ChatMatch {
    fn key(&self) -> (usize, usize) {
        (self.entry, self.range.start)
    }
}

#[derive(Default)]
pub(super) struct SearchState {
    matches: Vec<ChatMatch>,
    active: Option<usize>,
    highlighted: Vec<Entity<Markdown>>,
}

impl SearchState {
    pub(super) fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

impl ChatView {
    fn message_markdown(&self, entry: usize, cx: &App) -> Option<Entity<Markdown>> {
        match self.entries(cx).get(entry) {
            Some(Entry::Assistant { markdown, .. }) => Some(markdown.clone()),
            Some(Entry::User { .. }) => self.user_markdown.get(&entry).cloned(),
            _ => None,
        }
    }

    /// User messages get their text element on first render, after the search may have run.
    pub(super) fn highlight_new_message(
        &mut self,
        entry: usize,
        markdown: &Entity<Markdown>,
        cx: &mut App,
    ) {
        let Some(start) = self.search.matches.iter().position(|found| found.entry == entry) else {
            return;
        };
        let ranges: Vec<_> = self.search.matches[start..]
            .iter()
            .take_while(|found| found.entry == entry)
            .map(|found| found.range.clone())
            .collect();
        let active = self
            .search
            .active
            .and_then(|active| active.checked_sub(start))
            .filter(|active| *active < ranges.len());
        markdown.update(cx, |markdown, cx| markdown.set_search_highlights(ranges, active, cx));
        self.search.highlighted.push(markdown.clone());
    }

    fn turn_for_entry(&self, entry: usize) -> Option<usize> {
        self.turns
            .iter()
            .position(|turn| turn.user == Some(entry) || turn.items.contains(&entry))
    }

    pub(super) fn clear_search_highlights(&mut self, cx: &mut App) {
        for markdown in std::mem::take(&mut self.search.highlighted) {
            markdown.update(cx, |markdown, cx| markdown.clear_search_highlights(cx));
        }
    }

    fn top_visible_entry(&self) -> usize {
        let top = self
            .list
            .logical_scroll_top()
            .item_ix
            .min(self.turns.len().saturating_sub(1));
        self.turns
            .get(top)
            .and_then(|turn| turn.user.or_else(|| turn.items.first().copied()))
            .unwrap_or(0)
    }
}

impl SearchableItem for ChatView {
    type Match = ChatMatch;

    fn supported_options(&self) -> SearchOptions {
        SearchOptions {
            case: true,
            word: true,
            regex: true,
            replacement: false,
            selection: false,
            select_all: false,
            find_in_results: false,
        }
    }

    fn get_matches(&self, _: &mut Window, _: &mut App) -> (Vec<Self::Match>, SearchToken) {
        (self.search.matches.clone(), SearchToken::default())
    }

    fn clear_matches(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.search.matches.clear();
        self.search.active = None;
        self.clear_search_highlights(cx);
        cx.notify();
    }

    fn search_bar_visibility_changed(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !visible {
            self.clear_matches(window, cx);
        }
    }

    fn update_matches(
        &mut self,
        matches: &[Self::Match],
        active_match_index: Option<usize>,
        _: SearchToken,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search.matches = matches.to_vec();
        self.search.active = active_match_index;
        self.clear_search_highlights(cx);
        let mut start = 0;
        while let Some(first) = matches.get(start) {
            let end = start
                + matches[start..]
                    .iter()
                    .take_while(|found| found.entry == first.entry)
                    .count();
            if let Some(markdown) = self.message_markdown(first.entry, cx) {
                let ranges = matches[start..end]
                    .iter()
                    .map(|found| found.range.clone())
                    .collect::<Vec<_>>();
                let active = active_match_index
                    .filter(|active| (start..end).contains(active))
                    .map(|active| active - start);
                markdown.update(cx, |markdown, cx| {
                    markdown.set_search_highlights(ranges, active, cx)
                });
                self.search.highlighted.push(markdown);
            }
            start = end;
        }
        cx.notify();
    }

    fn query_suggestion(
        &mut self,
        _: Option<settings::SeedQuerySetting>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        self.entries(cx)
            .iter()
            .filter_map(|entry| match entry {
                Entry::Assistant { markdown, .. } => Some(markdown),
                _ => None,
            })
            .chain(self.user_markdown.values())
            .find(|markdown| markdown.focus_handle(cx).is_focused(window))
            .and_then(|markdown| markdown.read(cx).selected_source().map(str::to_owned))
            .filter(|selected| !selected.contains('\n'))
            .unwrap_or_default()
    }

    fn activate_match(
        &mut self,
        index: usize,
        matches: &[Self::Match],
        _: SearchToken,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = matches.get(index) else {
            return;
        };
        self.search.active = Some(index);
        self.list.pause_following_tail();
        let Some(turn) = self.turn_for_entry(found.entry) else {
            return;
        };
        let visible = self.container_bounds.get().is_some_and(|viewport| {
            self.painted_turns
                .borrow()
                .get(&turn)
                .is_some_and(|bounds| bounds.intersects(&viewport))
        });
        if !visible {
            self.list.scroll_to(ListOffset {
                item_ix: turn,
                offset_in_item: gpui::px(0.),
            });
        }
        if matches!(self.entries(cx).get(found.entry), Some(Entry::User { .. })) {
            self.expanded_users.insert(found.entry);
        }
        if let Some(markdown) = self.message_markdown(found.entry, cx) {
            let local = matches[..index]
                .iter()
                .rev()
                .take_while(|earlier| earlier.entry == found.entry)
                .count();
            let start = found.range.start;
            markdown.update(cx, |markdown, cx| {
                markdown.set_active_search_highlight(Some(local), cx);
                markdown.request_autoscroll_to_source_index(start, cx);
            });
        }
        cx.notify();
    }

    fn select_matches(
        &mut self,
        _: &[Self::Match],
        _: SearchToken,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn replace(
        &mut self,
        _: &Self::Match,
        _: &SearchQuery,
        _: SearchToken,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn find_matches(
        &mut self,
        query: Arc<SearchQuery>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<Self::Match>> {
        let sources: Vec<(usize, String, Vec<Range<usize>>)> = self
            .entries(cx)
            .iter()
            .enumerate()
            .filter_map(|(entry, item)| match item {
                Entry::User { text, .. } => Some((entry, text.to_string(), Vec::new())),
                Entry::Assistant { markdown, .. } => {
                    let markdown = markdown.read(cx);
                    let source = markdown.source();
                    let mut hidden = markdown.non_rendered_source_ranges();
                    hidden.extend(self.drawn_diagram_ranges(source, cx));
                    Some((entry, source.to_string(), merge_ranges(hidden)))
                }
                _ => None,
            })
            .collect();
        cx.background_spawn(async move {
            let mut found = Vec::new();
            for (entry, text, hidden) in sources {
                let mut ranges = query.search_str(&text);
                ranges.sort_by_key(|range| range.start);
                found.extend(
                    ranges
                        .into_iter()
                        .filter(|range| {
                            let candidate =
                                hidden.partition_point(|hidden| hidden.end <= range.start);
                            !hidden
                                .get(candidate)
                                .is_some_and(|hidden| hidden.start < range.end)
                        })
                        .map(|range| ChatMatch { entry, range }),
                );
            }
            found
        })
    }

    fn active_match_index(
        &mut self,
        direction: Direction,
        matches: &[Self::Match],
        _: SearchToken,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }
        let anchor = self
            .search
            .active
            .and_then(|active| self.search.matches.get(active))
            .map(ChatMatch::key)
            .unwrap_or((self.top_visible_entry(), 0));
        match direction {
            Direction::Next => matches
                .iter()
                .position(|found| found.key() >= anchor)
                .or(Some(0)),
            Direction::Prev => matches
                .iter()
                .rposition(|found| found.key() <= anchor)
                .or(Some(matches.len() - 1)),
        }
    }
}

fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::tests::{feed_events, feed_user, new_store},
    };
    use agent_harness::AgentEvent;
    use gpui::{TestAppContext, VisualTestContext, WeakEntity};
    use project::FakeFs;
    use util::paths::PathMatcher;

    #[gpui::test]
    async fn search_finds_messages_and_replies_and_jumps_to_matches(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        for turn in 0..60 {
            let question = if turn == 50 {
                "where is the needle".to_string()
            } else {
                format!("question {turn}")
            };
            feed_user(&session, &question, cx);
            let reply = if turn == 2 {
                "The **needle** is here.\n\nSecond paragraph.".to_string()
            } else {
                format!("Answer {turn}.\n\nSecond paragraph.\n\nThird paragraph.")
            };
            feed_events(&session, vec![AgentEvent::TextDelta { text: reply }], cx);
        }
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        let draw = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };
        draw(cx);

        let query = SearchQuery::text(
            "needle",
            false,
            false,
            false,
            PathMatcher::default(),
            PathMatcher::default(),
            false,
            None,
        )
        .expect("valid query");
        let found = chat
            .update_in(cx, |chat, window, cx| {
                chat.find_matches(Arc::new(query), window, cx)
            })
            .await;
        assert_eq!(
            found.len(),
            2,
            "one reply match and one message match: {found:?}"
        );
        let reply_entry = found[0].entry;
        let message_entry = found[1].entry;
        let reply_markdown = chat
            .read_with(cx, |chat, cx| chat.message_markdown(reply_entry, cx))
            .expect("first match is in a reply");
        assert_eq!(
            reply_markdown.read_with(cx, |markdown, _| markdown.source()[found[0].range.clone()]
                .to_string()),
            "needle"
        );
        assert!(chat.read_with(cx, |chat, cx| matches!(
            chat.entries(cx).get(message_entry),
            Some(Entry::User { .. })
        )));

        chat.update_in(cx, |chat, window, cx| {
            chat.update_matches(&found, Some(0), SearchToken::default(), window, cx);
            chat.activate_match(1, &found, SearchToken::default(), window, cx);
        });
        draw(cx);
        draw(cx);
        let message_turn = chat.read_with(cx, |chat, _| chat.turn_for_entry(message_entry));
        let painted = chat.read_with(cx, |chat, _| chat.painted_turns.borrow().clone());
        assert!(
            message_turn.is_some_and(|turn| painted.contains_key(&turn)),
            "the list scrolled to the message match"
        );
        assert_eq!(
            reply_markdown.read_with(cx, |markdown, _| markdown.search_highlights().len()),
            1
        );
        let message_markdown = chat
            .read_with(cx, |chat, cx| chat.message_markdown(message_entry, cx))
            .expect("a drawn message has selectable text");
        assert_eq!(
            message_markdown.read_with(cx, |markdown, _| (
                markdown.search_highlights().len(),
                markdown.active_search_highlight()
            )),
            (1, Some(0)),
            "a message drawn after the search still shows its match"
        );

        chat.update_in(cx, |chat, window, cx| {
            chat.activate_match(0, &found, SearchToken::default(), window, cx)
        });
        draw(cx);
        draw(cx);
        let reply_turn = chat.read_with(cx, |chat, _| chat.turn_for_entry(reply_entry));
        let painted = chat.read_with(cx, |chat, _| chat.painted_turns.borrow().clone());
        assert!(reply_turn.is_some_and(|turn| painted.contains_key(&turn)));
        assert_eq!(
            reply_markdown.read_with(cx, |markdown, _| markdown.active_search_highlight()),
            Some(0)
        );

        assert!(
            !chat.read_with(cx, |chat, _| chat.list.is_following_tail()),
            "jumping to a match stops following new output"
        );

        chat.update_in(cx, |chat, window, cx| chat.clear_matches(window, cx));
        assert!(
            reply_markdown.read_with(cx, |markdown, _| markdown.search_highlights().is_empty())
        );
        assert!(chat.read_with(cx, |chat, _| chat.search.is_empty()));

        chat.update_in(cx, |chat, window, cx| {
            chat.update_matches(&found, Some(0), SearchToken::default(), window, cx)
        });
        assert_eq!(
            reply_markdown.read_with(cx, |markdown, _| markdown.search_highlights().len()),
            1
        );
        drop(chat);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        assert!(
            reply_markdown.read_with(cx, |markdown, _| markdown.search_highlights().is_empty()),
            "closing the chat clears its highlights"
        );
    }

    #[test]
    fn hidden_ranges_merge_into_sorted_disjoint_spans() {
        assert_eq!(merge_ranges(vec![8..12, 0..3, 2..5, 12..14]), vec![0..5, 8..14]);
        assert_eq!(merge_ranges(Vec::new()), Vec::<Range<usize>>::new());
    }
}
