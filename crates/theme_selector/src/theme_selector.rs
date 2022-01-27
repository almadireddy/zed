use editor::{Editor, EditorSettings};
use fuzzy::{match_strings, StringMatch, StringMatchCandidate};
use gpui::{
    action,
    elements::*,
    keymap::{self, Binding},
    AppContext, Axis, Element, ElementBox, Entity, MutableAppContext, RenderContext, View,
    ViewContext, ViewHandle,
};
use parking_lot::Mutex;
use postage::watch;
use std::{cmp, sync::Arc};
use theme::ThemeRegistry;
use workspace::{
    menu::{Confirm, SelectNext, SelectPrev},
    AppState, Settings, Workspace,
};

#[derive(Clone)]
pub struct ThemeSelectorParams {
    pub settings_tx: Arc<Mutex<watch::Sender<Settings>>>,
    pub settings: watch::Receiver<Settings>,
    pub themes: Arc<ThemeRegistry>,
}

pub struct ThemeSelector {
    settings_tx: Arc<Mutex<watch::Sender<Settings>>>,
    settings: watch::Receiver<Settings>,
    themes: Arc<ThemeRegistry>,
    matches: Vec<StringMatch>,
    query_editor: ViewHandle<Editor>,
    list_state: UniformListState,
    selected_index: usize,
}

action!(Toggle, ThemeSelectorParams);
action!(Reload, ThemeSelectorParams);

pub fn init(params: ThemeSelectorParams, cx: &mut MutableAppContext) {
    cx.add_action(ThemeSelector::confirm);
    cx.add_action(ThemeSelector::select_prev);
    cx.add_action(ThemeSelector::select_next);
    cx.add_action(ThemeSelector::toggle);
    cx.add_action(ThemeSelector::reload);

    cx.add_bindings(vec![
        Binding::new("cmd-k cmd-t", Toggle(params.clone()), None),
        Binding::new("cmd-k t", Reload(params.clone()), None),
        Binding::new("escape", Toggle(params.clone()), Some("ThemeSelector")),
    ]);
}

pub enum Event {
    Dismissed,
}

impl ThemeSelector {
    fn new(
        settings_tx: Arc<Mutex<watch::Sender<Settings>>>,
        settings: watch::Receiver<Settings>,
        registry: Arc<ThemeRegistry>,
        cx: &mut ViewContext<Self>,
    ) -> Self {
        let query_editor = cx.add_view(|cx| {
            Editor::single_line(
                {
                    let settings = settings.clone();
                    Arc::new(move |_| {
                        let settings = settings.borrow();
                        EditorSettings {
                            tab_size: settings.tab_size,
                            style: settings.theme.selector.input_editor.as_editor(),
                            soft_wrap: editor::SoftWrap::None,
                        }
                    })
                },
                cx,
            )
        });

        cx.subscribe(&query_editor, Self::on_query_editor_event)
            .detach();

        let mut this = Self {
            settings,
            settings_tx,
            themes: registry,
            query_editor,
            matches: Vec::new(),
            list_state: Default::default(),
            selected_index: 0,
        };
        this.update_matches(cx);
        this
    }

    fn toggle(workspace: &mut Workspace, action: &Toggle, cx: &mut ViewContext<Workspace>) {
        workspace.toggle_modal(cx, |cx, _| {
            let selector = cx.add_view(|cx| {
                Self::new(
                    action.0.settings_tx.clone(),
                    action.0.settings.clone(),
                    action.0.themes.clone(),
                    cx,
                )
            });
            cx.subscribe(&selector, Self::on_event).detach();
            selector
        });
    }

    fn reload(_: &mut Workspace, action: &Reload, cx: &mut ViewContext<Workspace>) {
        let current_theme_name = action.0.settings.borrow().theme.name.clone();
        action.0.themes.clear();
        match action.0.themes.get(&current_theme_name) {
            Ok(theme) => {
                cx.refresh_windows();
                action.0.settings_tx.lock().borrow_mut().theme = theme;
                log::info!("reloaded theme {}", current_theme_name);
            }
            Err(error) => {
                log::error!("failed to load theme {}: {:?}", current_theme_name, error)
            }
        }
    }

    fn confirm(&mut self, _: &Confirm, cx: &mut ViewContext<Self>) {
        if let Some(mat) = self.matches.get(self.selected_index) {
            match self.themes.get(&mat.string) {
                Ok(theme) => {
                    self.settings_tx.lock().borrow_mut().theme = theme;
                    cx.refresh_windows();
                    cx.emit(Event::Dismissed);
                }
                Err(error) => log::error!("error loading theme {}: {}", mat.string, error),
            }
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, cx: &mut ViewContext<Self>) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
        self.list_state
            .scroll_to(ScrollTarget::Show(self.selected_index));
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, cx: &mut ViewContext<Self>) {
        if self.selected_index + 1 < self.matches.len() {
            self.selected_index += 1;
        }
        self.list_state
            .scroll_to(ScrollTarget::Show(self.selected_index));
        cx.notify();
    }

    fn update_matches(&mut self, cx: &mut ViewContext<Self>) {
        let background = cx.background().clone();
        let candidates = self
            .themes
            .list()
            .enumerate()
            .map(|(id, name)| StringMatchCandidate {
                id,
                char_bag: name.as_str().into(),
                string: name,
            })
            .collect::<Vec<_>>();
        let query = self.query_editor.update(cx, |buffer, cx| buffer.text(cx));

        self.matches = if query.is_empty() {
            candidates
                .into_iter()
                .enumerate()
                .map(|(index, candidate)| StringMatch {
                    candidate_id: index,
                    string: candidate.string,
                    positions: Vec::new(),
                    score: 0.0,
                })
                .collect()
        } else {
            smol::block_on(match_strings(
                &candidates,
                &query,
                false,
                100,
                &Default::default(),
                background,
            ))
        };
        cx.notify();
    }

    fn on_event(
        workspace: &mut Workspace,
        _: ViewHandle<ThemeSelector>,
        event: &Event,
        cx: &mut ViewContext<Workspace>,
    ) {
        match event {
            Event::Dismissed => {
                workspace.dismiss_modal(cx);
            }
        }
    }

    fn on_query_editor_event(
        &mut self,
        _: ViewHandle<Editor>,
        event: &editor::Event,
        cx: &mut ViewContext<Self>,
    ) {
        match event {
            editor::Event::Edited => self.update_matches(cx),
            editor::Event::Blurred => cx.emit(Event::Dismissed),
            _ => {}
        }
    }

    fn render_matches(&self, cx: &mut RenderContext<Self>) -> ElementBox {
        if self.matches.is_empty() {
            let settings = self.settings.borrow();
            return Container::new(
                Label::new(
                    "No matches".into(),
                    settings.theme.selector.empty.label.clone(),
                )
                .boxed(),
            )
            .with_style(settings.theme.selector.empty.container)
            .named("empty matches");
        }

        let handle = cx.handle();
        let list = UniformList::new(
            self.list_state.clone(),
            self.matches.len(),
            move |mut range, items, cx| {
                let cx = cx.as_ref();
                let selector = handle.upgrade(cx).unwrap();
                let selector = selector.read(cx);
                let start = range.start;
                range.end = cmp::min(range.end, selector.matches.len());
                items.extend(
                    selector.matches[range]
                        .iter()
                        .enumerate()
                        .map(move |(i, path_match)| selector.render_match(path_match, start + i)),
                );
            },
        );

        Container::new(list.boxed())
            .with_margin_top(6.0)
            .named("matches")
    }

    fn render_match(&self, theme_match: &StringMatch, index: usize) -> ElementBox {
        let settings = self.settings.borrow();
        let theme = &settings.theme;

        let container = Container::new(
            Label::new(
                theme_match.string.clone(),
                if index == self.selected_index {
                    theme.selector.active_item.label.clone()
                } else {
                    theme.selector.item.label.clone()
                },
            )
            .with_highlights(theme_match.positions.clone())
            .boxed(),
        )
        .with_style(if index == self.selected_index {
            theme.selector.active_item.container
        } else {
            theme.selector.item.container
        });

        container.boxed()
    }
}

impl Entity for ThemeSelector {
    type Event = Event;
}

impl View for ThemeSelector {
    fn ui_name() -> &'static str {
        "ThemeSelector"
    }

    fn render(&mut self, cx: &mut RenderContext<Self>) -> ElementBox {
        let settings = self.settings.borrow();

        Align::new(
            ConstrainedBox::new(
                Container::new(
                    Flex::new(Axis::Vertical)
                        .with_child(ChildView::new(&self.query_editor).boxed())
                        .with_child(Flexible::new(1.0, false, self.render_matches(cx)).boxed())
                        .boxed(),
                )
                .with_style(settings.theme.selector.container)
                .boxed(),
            )
            .with_max_width(600.0)
            .with_max_height(400.0)
            .boxed(),
        )
        .top()
        .named("theme selector")
    }

    fn on_focus(&mut self, cx: &mut ViewContext<Self>) {
        cx.focus(&self.query_editor);
    }

    fn keymap_context(&self, _: &AppContext) -> keymap::Context {
        let mut cx = Self::default_keymap_context();
        cx.set.insert("menu".into());
        cx
    }
}

impl<'a> From<&'a AppState> for ThemeSelectorParams {
    fn from(state: &'a AppState) -> Self {
        Self {
            settings_tx: state.settings_tx.clone(),
            settings: state.settings.clone(),
            themes: state.themes.clone(),
        }
    }
}
