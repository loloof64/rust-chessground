extern crate gtk;
extern crate gdk;
extern crate cairo;
extern crate rsvg;
extern crate shakmaty;
extern crate option_filter;

use std::cmp::{min, max};
use std::rc::Rc;
use std::cell::RefCell;
use std::f64::consts::PI;

use shakmaty::{Square, Color, Role, Piece, Board, Bitboard, MoveList, Position, Chess, Setup};

use gtk::prelude::*;
use gtk::DrawingArea;
use gdk::{EventButton, EventMotion};
use cairo::prelude::*;
use cairo::{Context, RadialGradient};
use rsvg::HandleExt;

use option_filter::OptionFilterExt;

use time::SteadyTime;
use rand;
use rand::distributions::{IndependentSample, Range};

use util;
use pieceset;
use drawable::Drawable;
use pieceset::PieceSet;

fn ease_in_out_cubic(start: f64, end: f64, elapsed: f64, duration: f64) -> f64 {
    let t = elapsed / duration;
    let ease = if t >= 1.0 {
        1.0
    } else if t >= 0.5 {
        (t - 1.0) * (2.0 * t - 2.0) * (2.0 * t - 2.0) + 1.0
    } else if t >= 0.0 {
        4.0 * t * t * t
    } else {
        0.0
    };
    start + (end - start) * ease
}

struct Figurine {
    square: Square,
    piece: Piece,
    pos: (f64, f64),
    time: SteadyTime,
    fading: bool,
    dragging: bool,
}

impl Figurine {
    fn pos(&self, now: SteadyTime) -> (f64, f64) {
        if self.dragging {
            (0.5 + self.square.file() as f64, 7.5 - self.square.rank() as f64)
        } else {
            let duration = 0.2;
            let (end_x, end_y) = util::square_to_inverted(self.square);

            (ease_in_out_cubic(self.pos.0, end_x, self.elapsed(now), duration),
             ease_in_out_cubic(self.pos.1, end_y, self.elapsed(now), duration))
        }
    }

    fn alpha(&self, now: SteadyTime) -> f64 {
        let base = if self.dragging { 0.2 } else { 1.0 };

        if self.fading {
            let duration = 0.2;
            base * ease_in_out_cubic(1.0, 0.0, self.elapsed(now), duration)
        } else {
            base
        }
    }

    fn elapsed(&self, now: SteadyTime) -> f64 {
        (now - self.time).num_milliseconds() as f64 / 1000.0
    }

    fn is_animating(&self, now: SteadyTime) -> bool {
        !self.dragging && self.elapsed(now) <= 0.2 &&
        (self.fading || self.pos != util::square_to_inverted(self.square))
    }

    fn queue_animation(&self, state: &BoardState, widget: &DrawingArea) {
        if self.is_animating(state.now) {
            let matrix = util::compute_matrix(widget, state.orientation);
            let pos = self.pos(state.now);

            let (x1, y1) = matrix.transform_point(pos.0 - 0.5, pos.1 - 0.5);
            let (x2, y2) = matrix.transform_point(pos.0 + 0.5, pos.1 + 0.5);
            let (x3, y3) = matrix.transform_point(self.square.file() as f64, 7.0 - self.square.rank() as f64);
            let (x4, y4) = matrix.transform_point(1.0 + self.square.file() as f64, 8.0 - self.square.rank() as f64);

            let xmin = min(
                min(x1.floor() as i32, x2.floor() as i32),
                min(x3.floor() as i32, x4.floor() as i32));
            let xmax = max(
                max(x1.ceil() as i32, x2.ceil() as i32),
                max(x3.ceil() as i32, x4.ceil() as i32));
            let ymin = min(
                min(y1.floor() as i32, y2.floor() as i32),
                min(y3.floor() as i32, y4.floor() as i32));
            let ymax = max(
                max(y1.ceil() as i32, y2.ceil() as i32),
                max(y3.ceil() as i32, y4.ceil() as i32));

            widget.queue_draw_area(xmin, ymin, xmax - xmin, ymax - ymin);
        }
    }

    fn render(&self, cr: &Context, state: &BoardState) {
        cr.push_group();

        let (x, y) = self.pos(state.now);
        cr.translate(x, y);
        cr.rotate(state.orientation.fold(0.0, PI));
        cr.translate(-0.5, -0.5);
        cr.scale(state.piece_set.scale(), state.piece_set.scale());

        state.piece_set.by_piece(&self.piece).render_cairo(cr);

        cr.pop_group_to_source();
        cr.paint_with_alpha(self.alpha(state.now));
    }
}

struct Pieces {
    board: Board,
    figurines: Vec<Figurine>,
}

impl Pieces {
    pub fn new() -> Pieces {
        Pieces::new_from_board(&Board::new())
    }

    pub fn new_from_board(board: &Board) -> Pieces {
        Pieces {
            board: board.clone(),
            figurines: board.occupied().map(|sq| Figurine {
                square: sq,
                piece: board.piece_at(sq).expect("enumerating"),
                pos: (0.5 + sq.file() as f64, 7.5 - sq.rank() as f64),
                time: SteadyTime::now(),
                fading: false,
                dragging: false,
            }).collect()
        }
    }

    pub fn set_board(&mut self, board: &Board) {
        let now = SteadyTime::now();

        // clean and freeze previous animation
        self.figurines.retain(|f| f.alpha(now) > 0.0001);
        for figurine in &mut self.figurines {
            if !figurine.fading {
                figurine.pos = figurine.pos(now);
                figurine.time = now;
            }
        }

        // diff
        let mut removed = Bitboard(0);
        let mut added = Vec::new();

        for square in self.board.occupied() | board.occupied() {
            let old = self.board.piece_at(square);
            let new = board.piece_at(square);
            if old != new {
                if old.is_some() {
                    removed.add(square);
                }
                if let Some(new) = new {
                    added.push((square, new));
                }
            }
        }

        // try to match additions and removals
        let mut matched = Vec::new();
        added.retain(|&(square, piece)| {
            let best = removed.filter(|sq| self.board.by_piece(piece).contains(*sq))
                              .min_by_key(|sq| sq.distance(square));

            if let Some(best) = best {
                removed.remove(best);
                matched.push((best, square));
                false
            } else {
                true
            }
        });

        for square in removed {
            for figurine in &mut self.figurines {
                if !figurine.fading && figurine.square == square {
                    figurine.fading = true;
                    figurine.time = now;
                }
            }
        }

        for (orig, dest) in matched {
            if let Some(figurine) = self.figurines.iter_mut().find(|f| !f.fading && f.square == orig) {
                figurine.square = dest;
                figurine.time = now;
            }
        }

        for (square, piece) in added {
            self.figurines.push(Figurine {
                square: square,
                piece: piece,
                pos: (0.5 + square.file() as f64, 7.5 - square.rank() as f64),
                time: now,
                fading: false,
                dragging: false,
            });
        }

        self.board = board.clone();
    }

    pub fn occupied(&self) -> Bitboard {
        self.figurines.iter().map(|f| f.square).collect()
    }

    pub fn render(&self, cr: &Context, state: &BoardState) {
        let now = SteadyTime::now();

        for figurine in &self.figurines {
            if figurine.fading || !figurine.is_animating(now) {
                figurine.render(cr, state);
            }
        }

        // draw currently animating pieces on top of others
        for figurine in &self.figurines {
            if !figurine.fading && figurine.is_animating(now) {
                figurine.render(cr, state);
            }
        }
    }

    pub fn figurine_at_mut(&mut self, square: Square) -> Option<&mut Figurine> {
        self.figurines.iter_mut().find(|f| !f.fading && f.square == square)
    }

    pub fn dragging(&self) -> Option<&Figurine> {
        self.figurines.iter().find(|f| f.dragging)
    }

    pub fn dragging_mut(&mut self) -> Option<&mut Figurine> {
        self.figurines.iter_mut().find(|f| f.dragging)
    }

    pub fn is_animating(&self, now: SteadyTime) -> bool {
        self.figurines.iter().any(|f| f.is_animating(now))
    }

    pub fn queue_animation(&self, state: &BoardState, widget: &DrawingArea) {
        for figurine in &self.figurines {
            figurine.queue_animation(state, widget);
        }
    }
}

struct BoardState {
    pieces: Pieces,
    orientation: Color,
    check: Option<Square>,
    selected: Option<Square>,
    last_move: Option<(Square, Square)>,
    piece_set: PieceSet,
    drawable: Drawable,
    now: SteadyTime,
    promoting: Option<Promoting>,
    legals: MoveList,
    pos: Chess,
}

impl BoardState {
    fn user_move(&mut self, orig: Square, dest: Square) {
        println!("user move: {} {}", orig, dest);

        let m = self.legals.drain(..).find(|m| m.from() == Some(orig) && m.to() == dest);
        if let Some(m) = m {
            self.pos = self.pos.clone().play_unchecked(&m);
            self.pieces.set_board(self.pos.board());
            self.last_move = Some((m.to(), m.from().unwrap_or_else(|| m.to())));

            // respond
            self.legals.clear();
            self.pos.legal_moves(&mut self.legals);
            if !self.legals.is_empty() {
                let mut rng = rand::thread_rng();
                let idx = Range::new(0, self.legals.len()).ind_sample(&mut rng);
                self.pos = self.pos.clone().play_unchecked(&self.legals[idx]);
                self.pieces.set_board(self.pos.board());
                self.last_move = Some((m.to(), m.from().unwrap_or_else(|| m.to())));
            }
        }

        self.legals.clear();
        self.pos.legal_moves(&mut self.legals);
        self.check = self.pos.board().king_of(self.pos.turn()).filter(|_| self.pos.checkers().any());
    }

    fn move_targets(&self, orig: Square) -> Bitboard {
        self.legals.iter().filter(|m| m.from() == Some(orig)).map(|m| m.to()).collect()
    }

    fn valid_move(&self, orig: Square, dest: Square) -> bool {
        self.move_targets(orig).contains(dest)
    }
}

struct Promoting {
    square: Square,
    hover: Option<Square>,
}

impl BoardState {
    fn new() -> Self {
        let pos = Chess::default();

        let mut state = BoardState {
            pieces: Pieces::new(),
            orientation: Color::White,
            check: None,
            last_move: None,
            selected: None,
            promoting: None,
            drawable: Drawable::new(),
            piece_set: pieceset::PieceSet::merida(),
            legals: MoveList::new(),
            pos: pos.clone(),
            now: SteadyTime::now(),
        };

        pos.legal_moves(&mut state.legals);

        state
    }
}

pub struct BoardView {
    widget: Rc<DrawingArea>,
    state: Rc<RefCell<BoardState>>,
}

impl BoardView {
    pub fn new() -> Self {
        let v = BoardView {
            widget: Rc::new(DrawingArea::new()),
            state: Rc::new(RefCell::new(BoardState::new())),
        };

        v.widget.add_events((gdk::BUTTON_PRESS_MASK |
                             gdk::BUTTON_RELEASE_MASK |
                             gdk::POINTER_MOTION_MASK).bits() as i32);

        {
            let weak_state = Rc::downgrade(&v.state);
            let weak_widget = Rc::downgrade(&v.widget);
            v.widget.connect_draw(move |widget, cr| {
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    state.now = SteadyTime::now();
                    let animating = state.pieces.is_animating(state.now);

                    let matrix = util::compute_matrix(widget, state.orientation);
                    cr.set_matrix(matrix);

                    draw_border(cr);
                    draw_board(cr, &state);
                    draw_check(cr, &state);
                    state.pieces.render(cr, &state);
                    state.drawable.render(cr);
                    draw_move_hints(cr, &state);
                    draw_drag(cr, &state);
                    draw_promoting(cr, &state);

                    let weak_state = weak_state.clone();
                    let weak_widget = weak_widget.clone();
                    if animating {
                        gtk::idle_add(move || {
                            if let (Some(state), Some(widget)) = (weak_state.upgrade(), weak_widget.upgrade()) {
                                let state = state.borrow();
                                state.pieces.queue_animation(&state, &widget);
                            }
                            Continue(false)
                        });
                    }
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&v.state);
            v.widget.connect_button_press_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    let square = util::pos_to_square(widget, state.orientation, e.get_position());

                    selection_mouse_down(&mut state, widget, e);
                    drag_mouse_down(&mut state, widget, square, e);
                    state.drawable.mouse_down(widget, square, e);
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&v.state);
            v.widget.connect_button_release_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    let square = util::pos_to_square(widget, state.orientation, e.get_position());

                    drag_mouse_up(&mut state, widget, square);
                    state.drawable.mouse_up(widget, square);
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&v.state);
            v.widget.connect_motion_notify_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    let square = util::pos_to_square(widget, state.orientation, e.get_position());

                    drag_mouse_move(&mut state, widget, square, e);
                    state.drawable.mouse_move(widget, square);
                }
                Inhibit(false)
            });
        }

        v
    }

    pub fn widget(&self) -> &DrawingArea {
        &self.widget
    }
}

fn selection_mouse_down(state: &mut BoardState, widget: &DrawingArea, e: &EventButton) {
    let orig = state.selected.take();

    if e.get_button() == 1 {
        let dest = util::pos_to_square(widget, state.orientation, e.get_position());

        state.selected = dest.filter(|sq| state.pieces.occupied().contains(*sq));

        if let (Some(orig), Some(dest)) = (orig, dest) {
            if state.valid_move(orig, dest) {
                state.selected = None;
                state.user_move(orig, dest);
            }
        }
    }

    widget.queue_draw();
}

fn drag_mouse_down(state: &mut BoardState, widget: &DrawingArea, square: Option<Square>, e: &EventButton) {
    if e.get_button() == 1 {
        if let Some(square) = square {
            if let Some(figurine) = state.pieces.figurine_at_mut(square) {
                figurine.pos = util::invert_pos(widget, state.orientation, e.get_position());
                figurine.time = SteadyTime::now();
                figurine.dragging = true;
            }

            widget.queue_draw();
        }
    }
}

fn queue_draw_square(widget: &DrawingArea, orientation: Color, square: Square) {
    let matrix = util::compute_matrix(widget, orientation);

    let (x1, y1) = matrix.transform_point(square.file() as f64, 7.0 - square.rank() as f64);
    let (x2, y2) = matrix.transform_point(1.0 + square.file() as f64, 8.0 - square.rank() as f64);

    let xmin = min(x1.floor() as i32, x2.floor() as i32);
    let ymin = min(y1.floor() as i32, y2.floor() as i32);
    let xmax = max(x1.ceil() as i32, x2.ceil() as i32);
    let ymax = max(y1.ceil() as i32, y2.ceil() as i32);

    widget.queue_draw_area(xmin, ymin, xmax - xmin, ymax - ymin);
}

fn drag_mouse_move(state: &mut BoardState, widget: &DrawingArea, square: Option<Square>, e: &EventMotion) {
    if let Some(dragging) = state.pieces.dragging_mut() {
        let matrix = util::compute_matrix(widget, state.orientation);

        // invalidate previous
        let (x, y) = matrix.transform_point(dragging.pos.0 - 0.5, dragging.pos.1 - 0.5);
        let (dx, dy) = matrix.transform_distance(1.0, 1.0);
        widget.queue_draw_area(x as i32, y as i32, dx as i32, dy as i32);
        queue_draw_square(widget, state.orientation, dragging.square);
        if let Some(sq) = util::inverted_to_square(dragging.pos) {
            queue_draw_square(widget, state.orientation, sq);
        }

        // update position
        dragging.pos = util::invert_pos(widget, state.orientation, e.get_position());

        // invalidate new
        let (x, y) = matrix.transform_point(dragging.pos.0 - 0.5, dragging.pos.1 - 0.5);
        let (dx, dy) = matrix.transform_distance(1.0, 1.0);
        widget.queue_draw_area(x as i32, y as i32, dx as i32, dy as i32);
        if let Some(sq) = square {
            queue_draw_square(widget, state.orientation, sq);
        }
    }
}

fn drag_mouse_up(state: &mut BoardState, widget: &DrawingArea, square: Option<Square>) {
    let m = if let Some(dragging) = state.pieces.dragging_mut() {
        widget.queue_draw();

        let dest = square.unwrap_or(dragging.square);
        dragging.pos = util::square_to_inverted(dest);
        dragging.time = SteadyTime::now();
        dragging.dragging = false;

        if dragging.square != dest {
            state.selected = None;
            Some((dragging.square, dest))
        } else {
            None
        }
    } else {
        None
    };

    if let Some((orig, dest)) = m {
        if state.valid_move(orig, dest) {
            state.user_move(orig, dest);
        }
    }
}

fn draw_border(cr: &Context) {
    let border = cairo::SolidPattern::from_rgb(0.2, 0.2, 0.5);
    cr.set_source(&border);
    cr.rectangle(-0.5, -0.5, 9.0, 9.0);
    cr.fill();
}

fn draw_board(cr: &Context, state: &BoardState) {
    let light = cairo::SolidPattern::from_rgb(0.87, 0.89, 0.90);
    let dark = cairo::SolidPattern::from_rgb(0.55, 0.64, 0.68);

    cr.rectangle(0.0, 0.0, 8.0, 8.0);
    cr.set_source(&dark);
    cr.fill();

    cr.set_source(&light);

    for square in Bitboard::all() {
        if square.is_light() {
            cr.rectangle(square.file() as f64, 7.0 - square.rank() as f64, 1.0, 1.0);
            cr.fill();
        }
    }

    if let Some(selected) = state.selected {
        cr.rectangle(selected.file() as f64, 7.0 - selected.rank() as f64, 1.0, 1.0);
        cr.set_source_rgba(0.08, 0.47, 0.11, 0.5);
        cr.fill();

        if let Some(hovered) = state.pieces.dragging().and_then(|d| util::inverted_to_square(d.pos)) {
            if state.valid_move(selected, hovered) {
                cr.rectangle(hovered.file() as f64, 7.0 - hovered.rank() as f64, 1.0, 1.0);
                cr.set_source_rgba(0.08, 0.47, 0.11, 0.25);
                cr.fill();
            }
        }
    }


    if let Some((orig, dest)) = state.last_move {
        cr.set_source_rgba(0.61, 0.78, 0.0, 0.41);
        cr.rectangle(orig.file() as f64, 7.0 - orig.rank() as f64, 1.0, 1.0);
        cr.fill();

        if dest != orig {
            cr.rectangle(dest.file() as f64, 7.0 - dest.rank() as f64, 1.0, 1.0);
            cr.fill();
        }
    }
}

fn draw_move_hints(cr: &Context, state: &BoardState) {
    if let Some(selected) = state.selected {
        cr.set_source_rgba(0.08, 0.47, 0.11, 0.5);

        let radius = 0.12;
        let corner = 1.8 * radius;

        for square in state.move_targets(selected) {
            if state.pieces.occupied().contains(square) {
                cr.move_to(square.file() as f64, 7.0 - square.rank() as f64);
                cr.rel_line_to(corner, 0.0);
                cr.rel_line_to(-corner, corner);
                cr.rel_line_to(0.0, -corner);
                cr.fill();

                cr.move_to(1.0 + square.file() as f64, 7.0 - square.rank() as f64);
                cr.rel_line_to(0.0, corner);
                cr.rel_line_to(-corner, -corner);
                cr.rel_line_to(corner, 0.0);
                cr.fill();

                cr.move_to(square.file() as f64, 8.0 - square.rank() as f64);
                cr.rel_line_to(corner, 0.0);
                cr.rel_line_to(-corner, -corner);
                cr.rel_line_to(0.0, corner);
                cr.fill();

                cr.move_to(1.0 + square.file() as f64, 8.0 - square.rank() as f64);
                cr.rel_line_to(-corner, 0.0);
                cr.rel_line_to(corner, -corner);
                cr.rel_line_to(0.0, corner);
                cr.fill();
            } else {
                cr.arc(0.5 + square.file() as f64,
                       7.5 - square.rank() as f64,
                       radius, 0.0, 2.0 * PI);
                cr.fill();
            }
        }
    }
}

fn draw_check(cr: &Context, state: &BoardState) {
    if let Some(check) = state.check {
        let cx = 0.5 + check.file() as f64;
        let cy = 7.5 - check.rank() as f64;
        let gradient = RadialGradient::new(cx, cy, 0.0, cx, cy, 0.5f64.hypot(0.5));
        gradient.add_color_stop_rgba(0.0, 1.0, 0.0, 0.0, 1.0);
        gradient.add_color_stop_rgba(0.25, 0.91, 0.0, 0.0, 1.0);
        gradient.add_color_stop_rgba(0.89, 0.66, 0.0, 0.0, 0.0);
        cr.set_source(&gradient);
        cr.paint();
    }
}

fn draw_drag(cr: &Context, state: &BoardState) {
    if let Some(dragging) = state.pieces.dragging() {
        cr.save();
        cr.translate(dragging.pos.0, dragging.pos.1);
        cr.rotate(state.orientation.fold(0.0, PI));
        cr.translate(-0.5, -0.5);
        cr.scale(state.piece_set.scale(), state.piece_set.scale());
        state.piece_set.by_piece(&dragging.piece).render_cairo(cr);
        cr.restore();
    }
}

fn draw_promoting(cr: &Context, state: &BoardState) {
    if let Some(ref promoting) = state.promoting {
        let mut square = promoting.square;

        cr.rectangle(0.0, 0.0, 8.0, 8.0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        cr.fill();

        let offset = if square.rank() < 4 { -1.0 } else { 1.0 };
        let mut y = 7.0 - square.rank() as f64;
        let mut light = square.is_light();

        for role in &[Role::Queen, Role::Rook, Role::Bishop, Role::Knight] {
            if square.is_light() {
                cr.set_source_rgb(0.25, 0.25, 0.25);
            } else {
                cr.set_source_rgb(0.18, 0.18, 0.18);
            }
            cr.rectangle(square.file() as f64, y, 1.0, 1.0);
            cr.fill();

            if promoting.hover == Some(square) {
                cr.set_source_rgb(1.0, 0.65, 0.0);
            } else {
                cr.set_source_rgb(0.69, 0.69, 0.69);
            }
            cr.arc(0.5 + square.file() as f64, y + 0.5 * offset,
                   0.5, 0.0, 2.0 * PI);
            cr.fill();

            cr.save();
            cr.translate(0.5 + square.file() as f64, y + 0.5 * offset);
            cr.scale(0.707, 0.707);
            cr.translate(-0.5, -0.5);
            cr.scale(0.0056, 0.0056);
            state.piece_set.by_piece(&role.of(Color::White)).render_cairo(cr);
            cr.restore();

            y += offset;
            light = !light;
            square = Square::from_coords(square.file(), square.rank() - offset as i8).expect("promotion dialog square on board");
        }
    }
}