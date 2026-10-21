use core::ops::Range;

use super::Clock;
use crate::plugin::{commands, names, order};
use crate::{
    Align, ButtonKind, CandleView, Chart, Clip, Color, DrawCtx, Event, EventCtx, Handler, Key, Label, Layer, Plugin,
    Resources, Response, Time, ToolButton, TradeView,
};

/// One executed virtual order. `qty` is signed: positive buys, negative sells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    pub time: Time,
    pub price: f64,
    pub qty: f64,
    /// What the fill cost in fees, in the quote currency.
    pub fee: f64,
}

/// When a pending order triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    /// Buys at the price or lower, sells at the price or higher.
    Limit,
    /// Buys once the price rises to it, sells once the price falls to it.
    Stop,
}

/// An order waiting for the market. `qty` is signed like a fill's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Order {
    pub kind: OrderKind,
    pub qty: f64,
    pub price: f64,
    /// Placed when this bar was the newest one: only later bars can fill it, as a person clicking
    /// on a chart cannot trade inside a bar that is already drawn. On a live chart outside a replay, the moment it
    /// was placed: only the trades after it fill it (see [`Chart::order_after`]).
    pub after: Time,
    /// Only ever closes (part of) the position: it fills no more than what is open, and is dropped once
    /// nothing is left to close. A stop loss and a take profit placed on a position work as a pair this way:
    /// when one fills, the other goes.
    pub reduce_only: bool,
    /// Put on the position once this order fills.
    pub exits: Exits,
    /// A stop loss's break-even: once the market reaches this price, the stop moves to the entry of the position.
    pub break_even: Option<f64>,
}

/// A take profit and a stop loss for the position an order opens, and where the stop moves to the entry.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Exits {
    pub take_profit: Option<f64>,
    pub stop_loss: Option<f64>,
    /// See [`Order::break_even`].
    pub break_even: Option<f64>,
}

impl Order {
    /// The price at which this order fills on a bar, if the bar reaches it. A bar that opens beyond
    /// the order (a gap) fills at the open, which is what the market would have given.
    fn fill_price(&self, open: f64, high: f64, low: f64) -> Option<f64> {
        let buy = self.qty > 0.0;
        match (self.kind, buy) {
            (OrderKind::Limit, true) | (OrderKind::Stop, false) => (low <= self.price).then(|| self.price.min(open)),
            (OrderKind::Limit, false) | (OrderKind::Stop, true) => (high >= self.price).then(|| self.price.max(open)),
        }
    }

    /// The price at which this order fills on a trade at `price`, if the trade reaches it: a limit at its own
    /// price, a stop at the trade's.
    fn trade_fill(&self, price: f64) -> Option<f64> {
        let fill = self.fill_price(price, price, price)?;
        Some(if self.kind == OrderKind::Limit {
            self.price
        } else {
            fill
        })
    }
}

/// The starting balance of a new account.
pub const DEFAULT_BALANCE: f64 = 10_000.0;

/// The virtual account: every fill, and the position, its average price and the realized profit
/// that follow from them. Kept as a resource, so any plugin (a strategy readout, an export) can read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    /// Size of one order.
    pub order_size: f64,
    /// Fee per fill as a fraction of its value (`0.001` is 0.1%).
    pub fee_rate: f64,
    /// How much worse than its price a market or stop order fills, as a fraction of it (`0.0005` is 0.05%): the
    /// price moves while such an order is done. A limit order fills at its own price.
    pub slippage: f64,
    /// What the account starts with, in the quote currency.
    pub balance: f64,
    fills: Vec<Fill>,
    orders: Vec<Order>,
    fees: f64,
    /// The fees paid for what is still open, charged to the trades that close it.
    open_fees: f64,
    position: f64,
    average: f64,
    realized: f64,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            order_size: 1.0,
            fee_rate: 0.0,
            slippage: 0.0,
            balance: DEFAULT_BALANCE,
            fills: Vec::new(),
            orders: Vec::new(),
            fees: 0.0,
            open_fees: 0.0,
            position: 0.0,
            average: 0.0,
            realized: 0.0,
        }
    }
}

impl Account {
    pub fn fills(&self) -> &[Fill] {
        &self.fills
    }

    /// Signed size of the open position: positive is long, negative short, zero flat.
    pub fn position(&self) -> f64 {
        self.position
    }

    /// Average entry price of the open position.
    pub fn average_price(&self) -> Option<f64> {
        (self.position != 0.0).then_some(self.average)
    }

    /// Profit of everything closed so far, before fees.
    pub fn realized(&self) -> f64 {
        self.realized
    }

    /// Fees paid so far.
    pub fn fees(&self) -> f64 {
        self.fees
    }

    /// Total profit if the position were closed at `price`, after the fees paid so far.
    pub fn net(&self, price: f64) -> f64 {
        self.realized + self.unrealized(price) - self.fees
    }

    /// What the account is worth if the position were closed at `price`: the starting balance and the net.
    pub fn equity(&self, price: f64) -> f64 {
        self.balance + self.net(price)
    }

    /// Orders waiting for the market, oldest first.
    pub fn orders(&self) -> &[Order] {
        &self.orders
    }

    /// Puts an order in the book. Nonsense (no size, no price) is ignored.
    pub fn place(&mut self, order: Order) {
        if order.qty != 0.0 && order.qty.is_finite() && order.price.is_finite() && order.price > 0.0 {
            self.orders.push(order);
        }
    }

    /// Moves a pending order to another price. Returns whether it did (the index exists and the price is sane).
    pub fn move_order(&mut self, index: usize, price: f64) -> bool {
        match self.orders.get_mut(index) {
            Some(order) if price.is_finite() && price > 0.0 => {
                order.price = price;
                true
            }
            _ => false,
        }
    }

    /// Sets or takes off the break-even of a stop loss. Returns whether it did (the order is a stop loss and the
    /// price is sane).
    pub fn set_break_even(&mut self, index: usize, trigger: Option<f64>) -> bool {
        match self.orders.get_mut(index) {
            Some(order)
                if order.reduce_only
                    && order.kind == OrderKind::Stop
                    && trigger.is_none_or(|t| t.is_finite() && t > 0.0) =>
            {
                order.break_even = trigger;
                true
            }
            _ => false,
        }
    }

    /// Closes an entry of `qty` (signed like a fill) at its exits: reduce-only orders that fill on bars after
    /// `after`, a limit at the take profit and a stop at the stop loss (with its break-even). When one fills, the
    /// other goes.
    pub fn protect(&mut self, after: Time, qty: f64, exits: Exits) {
        let exit = |kind, price| Order {
            kind,
            qty: -qty,
            price,
            after,
            reduce_only: true,
            exits: Exits::default(),
            break_even: None,
        };
        if let Some(price) = exits.take_profit {
            self.place(exit(OrderKind::Limit, price));
        }
        if let Some(price) = exits.stop_loss {
            self.place(Order {
                break_even: exits.break_even,
                ..exit(OrderKind::Stop, price)
            });
        }
    }

    /// Moves to the entry each stop loss whose break-even the market reached: `high` and `low` of what came at
    /// `at`, after the stop was placed.
    fn reach_break_even(&mut self, at: Time, high: f64, low: f64) {
        if self.position == 0.0 {
            return;
        }
        let average = self.average;
        for order in &mut self.orders {
            let Some(trigger) = order.break_even.filter(|_| order.after < at) else {
                continue;
            };
            // A sell stop guards a long: a rise to the trigger sets it.
            if (order.qty < 0.0 && high >= trigger) || (order.qty > 0.0 && low <= trigger) {
                order.price = average;
                order.break_even = None;
            }
        }
    }

    /// Takes one pending order off the book. Returns it, if the index exists.
    pub fn cancel_order(&mut self, index: usize) -> Option<Order> {
        (index < self.orders.len()).then(|| self.orders.remove(index))
    }

    pub fn cancel_all(&mut self) {
        self.orders.clear();
    }

    /// Fills the orders that a bar reaches, stops first, then oldest first, and returns how many were filled.
    /// Orders placed on this bar or later ones (`after >= time`) are not touched.
    pub fn match_bar(&mut self, time: Time, open: f64, high: f64, low: f64) -> usize {
        // Which of the bar's prices came first is unknown, so the worse is taken: a position whose stop and target
        // are both on the bar is stopped out. The exits wait for the bars after this one.
        let reach = |order: &Order| order.fill_price(open, high, low);
        let stops = self.match_orders(time, time, time, self.orders.len(), |order| {
            reach(order).filter(|_| order.kind == OrderKind::Stop)
        });
        let filled = stops + self.match_orders(time, time, time, self.orders.len(), reach);
        // So does a stop moved to the entry: the bar may have come back to it before it went to the trigger.
        self.reach_break_even(time, high, low);
        filled
    }

    /// Like [`Account::match_bar`], but the bar's own `trades` (oldest first) say in which order its prices came:
    /// a stop and a target both on the bar fill as the market reached them, and the exits of an entry work from
    /// the trade after it. A limit fills at its price (it waited in the book), a stop at the trade that set it
    /// off, and a stop whose break-even a trade reaches is at the entry for the trades after. Fills keep the bar's
    /// time. Without trades, or when no order or break-even is within the bar's range, it is [`Account::match_bar`].
    pub fn match_tape(&mut self, time: Time, open: f64, high: f64, low: f64, trades: TradeView<'_>) -> usize {
        let within = |p: f64| low <= p && p <= high;
        let reached = self
            .orders
            .iter()
            .any(|o| o.after < time && (o.fill_price(open, high, low).is_some() || o.break_even.is_some_and(within)));
        if trades.is_empty() || !reached {
            return self.match_bar(time, open, high, low);
        }
        self.match_trades(time, trades)
    }

    /// Fills the orders that `trades` (oldest first) of the bar at `time` reach, one trade after another: the
    /// trades of a bar that came after those already matched, such as the newest of a bar still forming.
    pub fn match_trades(&mut self, time: Time, trades: TradeView<'_>) -> usize {
        // Exits placed during the bar count as placed before it, so its later trades reach them.
        let exits_after = Time(time.0 - 1);
        let mut filled = 0;
        for &price in trades.price() {
            if self.orders.is_empty() {
                break;
            }
            // The exits a fill places wait for the next trade.
            filled += self.match_orders(time, time, exits_after, self.orders.len(), |order| {
                order.trade_fill(price)
            });
            self.reach_break_even(time, price, price);
        }
        filled
    }

    /// Fills the orders that live `trades` (oldest first) reach, each by the trades after the moment it was placed
    /// (its `after`), and places their exits after the trade that filled them. A fill keeps the time of the bar its
    /// trade is in (of `bars`, the bars' times); a trade before the first bar fills nothing.
    pub fn match_live(&mut self, trades: TradeView<'_>, bars: &[Time]) -> usize {
        let mut filled = 0;
        for (&at, &price) in trades.time().iter().zip(trades.price()) {
            if self.orders.is_empty() {
                break;
            }
            let Some(bar) = bars.partition_point(|&b| b <= at).checked_sub(1).map(|i| bars[i]) else {
                continue;
            };
            filled += self.match_orders(bar, at, at, self.orders.len(), |order| order.trade_fill(price));
            self.reach_break_even(at, price, price);
        }
        filled
    }

    /// Fills, at `time`, those of the first `count` orders placed before `at` that `reach` gives a price for, oldest
    /// first, and places the exits of what fills after `exits_after`.
    fn match_orders(
        &mut self,
        time: Time,
        at: Time,
        exits_after: Time,
        count: usize,
        reach: impl Fn(&Order) -> Option<f64>,
    ) -> usize {
        let mut filled = 0;
        let mut i = 0;
        for _ in 0..count {
            let order = self.orders[i];
            if order.reduce_only && self.position * order.qty >= 0.0 {
                // Nothing to close (the position is gone, or is on the other side now).
                self.orders.remove(i);
            } else if order.after < at
                && let Some(price) = reach(&order)
            {
                self.orders.remove(i);
                let qty = if order.reduce_only {
                    order.qty.signum() * order.qty.abs().min(self.position.abs())
                } else {
                    order.qty
                };
                // A stop is a market order once set off: it slips.
                match order.kind {
                    OrderKind::Limit => self.fill(time, price, qty),
                    OrderKind::Stop => self.market(time, price, qty),
                };
                self.protect(exits_after, qty, order.exits);
                filled += 1;
            } else {
                i += 1;
            }
        }
        if filled > 0 {
            // An exit that came before the fill that closed the position goes with it too.
            let position = self.position;
            self.orders.retain(|o| !(o.reduce_only && position * o.qty >= 0.0));
        }
        filled
    }

    /// Profit of the open position if it were closed at `price`.
    pub fn unrealized(&self, price: f64) -> f64 {
        self.position * (price - self.average)
    }

    /// An account with this history, as a saved session had it: the fills (each with the fee it paid) make the
    /// position, its average and the result; the orders wait as they did.
    pub fn restore(order_size: f64, fee_rate: f64, fills: &[Fill], orders: &[Order]) -> Self {
        let mut account = Self {
            order_size,
            fee_rate,
            ..Self::default()
        };
        for &fill in fills {
            account.record(fill);
        }
        for &order in orders {
            account.place(order);
        }
        account
    }

    /// Records an order filled at `price`. Returns the result of the trade if it closed some of the position:
    /// the profit after the fees of opening and closing that part. `None` if it only opened or added to one.
    pub fn fill(&mut self, time: Time, price: f64, qty: f64) -> Option<f64> {
        let fee = price * qty.abs() * self.fee_rate;
        self.record(Fill { time, price, qty, fee })
    }

    /// Records a market order at `price` as it fills: worse by the slippage, higher for a buy, lower for a sell.
    pub fn market(&mut self, time: Time, price: f64, qty: f64) -> Option<f64> {
        self.fill(time, price * (1.0 + self.slippage * qty.signum()), qty)
    }

    fn record(&mut self, fill: Fill) -> Option<f64> {
        let Fill { price, qty, fee, .. } = fill;
        if qty == 0.0 || !price.is_finite() || !qty.is_finite() || !fee.is_finite() {
            return None;
        }
        self.fees += fee;
        self.fills.push(fill);
        let same_side = self.position == 0.0 || self.position.signum() == qty.signum();
        if same_side {
            let total = self.position.abs() + qty.abs();
            self.average = (self.average * self.position.abs() + price * qty.abs()) / total;
            self.position += qty;
            self.open_fees += fee;
            return None;
        }
        let closed = qty.abs().min(self.position.abs());
        let profit = closed * (price - self.average) * self.position.signum();
        let entry_fees = self.open_fees * closed / self.position.abs();
        let exit_fee = fee * closed / qty.abs();
        self.open_fees -= entry_fees;
        self.realized += profit;
        self.position = add_position(self.position, qty);
        if self.position == 0.0 {
            self.position = 0.0;
            self.average = 0.0;
            self.open_fees = 0.0;
        } else if qty.signum() == self.position.signum() {
            // Flipped through zero: the rest is a new position at this price, which paid the rest of the fee.
            self.average = price;
            self.open_fees = fee - exit_fee;
        }
        Some(profit - entry_fees - exit_fee)
    }

    /// The result of each of `fills` (see [`Account::fill`]): `None` for those that only opened or added.
    pub fn results(fills: &[Fill]) -> Vec<Option<f64>> {
        let mut sim = Self::default();
        fills.iter().map(|&fill| sim.record(fill)).collect()
    }

    /// A copy with the same settings (size, fee rate, slippage, balance) and no history.
    pub fn empty_like(&self) -> Self {
        Self {
            order_size: self.order_size,
            fee_rate: self.fee_rate,
            slippage: self.slippage,
            balance: self.balance,
            ..Self::default()
        }
    }

    /// How the fills so far went. Computed by replaying them, so it is always consistent with `fills()`.
    pub fn stats(&self) -> Stats {
        let mut sim = self.empty_like();
        let mut stats = Stats::default();
        let mut peak = 0.0_f64;
        for fill in &self.fills {
            if let Some(profit) = sim.record(*fill) {
                stats.add(profit);
            }
            let net = sim.realized - sim.fees;
            peak = peak.max(net);
            stats.max_drawdown = stats.max_drawdown.max(peak - net);
            let top = self.balance + peak;
            if top > 0.0 {
                stats.max_drawdown_share = stats.max_drawdown_share.max((peak - net) / top);
            }
        }
        stats
    }

    /// The equity (`net`, marked to each bar's close) at the bars of `range`, from the fills so far:
    /// what the account was worth as the chart went by. Uses only bars of `view`, so it stops at the replay cursor.
    pub fn equity_curve(&self, view: CandleView<'_>, range: Range<usize>) -> Vec<(usize, f64)> {
        let range = range.start.min(view.len())..range.end.min(view.len());
        let (time, close) = (view.time(), view.close());
        let mut sim = self.empty_like();
        let mut fills = self.fills.iter().peekable();
        let mut out = Vec::with_capacity(range.len());
        for i in range.clone() {
            // A bar counts the fills up to its end, which is the start of the next bar.
            let end = time.get(i + 1).copied();
            while let Some(fill) = fills.next_if(|f| end.is_none_or(|end| f.time < end)) {
                sim.record(*fill);
            }
            out.push((i, sim.net(close[i])));
        }
        out
    }
}

/// Summary of the closed trades of an [`Account`]. A trade is a fill that closes (part of) a position.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub closed: usize,
    pub wins: usize,
    /// Total of the winning trades, after their fees.
    pub gross_profit: f64,
    /// Total of the losing trades after their fees, as a positive number.
    pub gross_loss: f64,
    /// The largest fall of realized profit after fees from a previous peak (a positive number).
    pub max_drawdown: f64,
    /// The largest such fall as a share of the balance at its peak (`0.05` is 5%).
    pub max_drawdown_share: f64,
    /// The most wins in a row, and the most losses (a trade that made nothing is a loss).
    pub win_streak: usize,
    pub loss_streak: usize,
    /// The trades in a row so far: wins above zero, losses below.
    run: i64,
}

impl Stats {
    /// Counts a closed trade that made `profit` after its fees.
    pub fn add(&mut self, profit: f64) {
        self.closed += 1;
        if profit > 0.0 {
            self.wins += 1;
            self.gross_profit += profit;
            self.run = self.run.max(0) + 1;
        } else {
            self.gross_loss += -profit;
            self.run = self.run.min(0) - 1;
        }
        self.win_streak = self.win_streak.max(self.run.max(0) as usize);
        self.loss_streak = self.loss_streak.max((-self.run).max(0) as usize);
    }

    /// Adds those of other trades: the longest streaks are the longer of the two.
    pub fn merge(&mut self, other: &Stats) {
        self.closed += other.closed;
        self.wins += other.wins;
        self.gross_profit += other.gross_profit;
        self.gross_loss += other.gross_loss;
        self.win_streak = self.win_streak.max(other.win_streak);
        self.loss_streak = self.loss_streak.max(other.loss_streak);
    }

    /// Share of winning trades, `None` before the first.
    pub fn win_rate(&self) -> Option<f64> {
        (self.closed > 0).then(|| self.wins as f64 / self.closed as f64)
    }

    /// Gross profit over gross loss; `None` without losses (it would be infinite) or without trades.
    pub fn profit_factor(&self) -> Option<f64> {
        (self.gross_loss > 0.0).then(|| self.gross_profit / self.gross_loss)
    }

    /// What a trade made on average, after fees: what the next one is worth if they go on as they went.
    pub fn expectancy(&self) -> Option<f64> {
        (self.closed > 0).then(|| (self.gross_profit - self.gross_loss) / self.closed as f64)
    }

    /// The average win and the average loss (a positive number), `None` without any.
    pub fn average_win(&self) -> Option<f64> {
        (self.wins > 0).then(|| self.gross_profit / self.wins as f64)
    }

    pub fn average_loss(&self) -> Option<f64> {
        let losses = self.closed - self.wins;
        (losses > 0).then(|| self.gross_loss / losses as f64)
    }
}

/// The stops and targets [`exits_tried`] tries, as shares of the entry price (`0.01` is 1%).
pub const EXIT_SHARES: [f64; 14] = [
    0.001, 0.002, 0.003, 0.005, 0.0075, 0.01, 0.015, 0.02, 0.03, 0.05, 0.075, 0.1, 0.15, 0.2,
];

/// What the round trips of some fills made, and what they would have made with other exits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExitsTried {
    /// The round trips closed.
    pub trades: usize,
    /// What they made as traded: each its result before fees as a share of its entry, added up.
    pub as_traded: f64,
    /// Each stop and target of [`EXIT_SHARES`] and what the trades would have made with them, measured the same
    /// way; the best first.
    pub tried: Vec<TriedExit>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriedExit {
    pub stop: f64,
    pub target: f64,
    pub total: f64,
}

/// Tries a stop and a target at each pair of [`EXIT_SHARES`] on the round trips of `fills` (a position from flat
/// back to flat, or to the other side): each would have gone out at the first of them that the bars after its entry's
/// bar reached (the stop when a bar reached both), or at the last close of `bars` when neither. Only `bars` count,
/// so in a replay nothing past the cursor does. A trip is measured at its first fill's price and its largest size.
pub fn exits_tried(fills: &[Fill], bars: CandleView<'_>) -> ExitsTried {
    let n = EXIT_SHARES.len();
    let mut totals = vec![0.0; n * n];
    let mut out = ExitsTried::default();
    let mut sim = Account::default();
    // The trip going on: its first fill, its largest size and its result before fees so far.
    let mut trip: Option<(Fill, f64, f64)> = None;
    for &fill in fills {
        let before = sim.position;
        let profit = sim.record(Fill { fee: 0.0, ..fill });
        let after = sim.position;
        let closes = before != 0.0 && (after == 0.0 || after.signum() != before.signum());
        if let Some((_, size, made)) = &mut trip {
            if !closes {
                *size = size.max(after.abs());
            }
            *made += profit.unwrap_or(0.0);
        }
        if closes && let Some((entry, size, made)) = trip.take() {
            out.trades += 1;
            out.as_traded += made / (entry.price * size);
            try_exits(&entry, bars, &mut totals);
        }
        if after != 0.0 && (before == 0.0 || after.signum() != before.signum()) {
            trip = Some((Fill { qty: after, ..fill }, after.abs(), 0.0));
        }
    }
    out.tried = totals
        .iter()
        .enumerate()
        .map(|(i, &total)| TriedExit {
            stop: EXIT_SHARES[i / n],
            target: EXIT_SHARES[i % n],
            total,
        })
        .collect();
    out.tried.sort_by(|a, b| b.total.total_cmp(&a.total));
    out
}

/// Adds to `totals` (a row of targets per stop) what the trip entered by `entry` would have made with each pair.
fn try_exits(entry: &Fill, bars: CandleView<'_>, totals: &mut [f64]) {
    const N: usize = EXIT_SHARES.len();
    let (price, side) = (entry.price, entry.qty.signum());
    let (high, low, close) = (bars.high(), bars.low(), bars.close());
    // The first bar that reached each stop and each target; the nearest are reached first.
    let (mut stop_at, mut target_at) = ([usize::MAX; N], [usize::MAX; N]);
    let (mut stops, mut targets) = (0, 0);
    for i in bars.time().partition_point(|&t| t <= entry.time)..bars.len() {
        let (up, down) = (high[i] / price - 1.0, 1.0 - low[i] / price);
        // A bar at the level reaches it, though the division may land a hair short.
        let (against, with) = if side > 0.0 { (down, up) } else { (up, down) };
        let (against, with) = (against + 1e-12, with + 1e-12);
        while stops < N && against >= EXIT_SHARES[stops] {
            stop_at[stops] = i;
            stops += 1;
        }
        while targets < N && with >= EXIT_SHARES[targets] {
            target_at[targets] = i;
            targets += 1;
        }
        if stops == N && targets == N {
            break;
        }
    }
    let last = close.last().map_or(0.0, |c| side * (c / price - 1.0));
    for (s, &stop) in EXIT_SHARES.iter().enumerate() {
        for (t, &target) in EXIT_SHARES.iter().enumerate() {
            totals[s * N + t] += if stop_at[s] != usize::MAX && stop_at[s] <= target_at[t] {
                -stop
            } else if target_at[t] != usize::MAX {
                target
            } else {
                last
            };
        }
    }
}

/// Marks a chart whose data come live from the market: outside a replay its orders are paper trades, filled by the
/// trades that come after they were placed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Live(pub bool);

/// `position` after a fill of `qty`: flat when what is left is float noise of the sizes added and taken.
pub fn add_position(position: f64, qty: f64) -> f64 {
    let after = position + qty;
    if after.abs() <= 1e-9 * position.abs().max(qty.abs()) {
        0.0
    } else {
        after
    }
}

fn live(res: &Resources) -> bool {
    res.get::<Live>().is_some_and(|l| l.0)
}

/// Whether orders can be placed: in a replay, or on a live chart.
pub fn can_trade(res: &Resources) -> bool {
    replaying(res) || live(res)
}

/// When an order placed now counts as placed (its `after`): on a bar still forming, just before the bar, so its
/// trades to come fill it; on a live chart outside a replay, now by the wall clock ([`Clock`]), so the trades after
/// now do; else on the newest bar, so the bars after it do.
pub(crate) fn order_after(newest: Time, forming: bool, replaying: bool, res: &Resources) -> Time {
    if forming {
        return Time(newest.0 - 1);
    }
    match res.get::<Clock>() {
        Some(&Clock(now)) if !replaying && live(res) => now,
        _ => newest,
    }
}

/// The trades of bar `i` of the bars opening at `time`: from its open to the next bar's (the last bar's run to the
/// end of `trades`).
pub(crate) fn bar_trades<'a>(trades: TradeView<'a>, time: &[Time], i: usize) -> TradeView<'a> {
    let end = time.get(i + 1).copied().unwrap_or(Time(i64::MAX));
    trades.between(time[i], end)
}

/// The order size after `size` in 1-2-5 steps (0.5, 1, 2, 5, 10...). A size that is not a number
/// or not positive becomes 1.
pub fn size_up(size: f64) -> f64 {
    step_size(size, true)
}

/// The order size before `size` in 1-2-5 steps.
pub fn size_down(size: f64) -> f64 {
    step_size(size, false)
}

fn step_size(size: f64, up: bool) -> f64 {
    if !(size.is_finite() && size > 0.0) {
        return 1.0;
    }
    let decade = size.log10().floor() as i32;
    let ladder = (decade - 1..=decade + 1).flat_map(|d| [1.0, 2.0, 5.0].map(|m| m * 10f64.powi(d)));
    let stepped = if up {
        ladder
            .filter(|&s| s > size * (1.0 + 1e-9))
            .fold(f64::INFINITY, f64::min)
    } else {
        ladder.filter(|&s| s < size * (1.0 - 1e-9)).fold(0.0, f64::max)
    };
    // Trim the noise of the multiplication (0.1 * 5 is not exactly 0.5).
    let stepped: f64 = format!("{stepped:.10e}").parse().unwrap_or(stepped);
    if stepped.is_finite() && stepped > 0.0 {
        stepped
    } else {
        size
    }
}

/// `B` buys and `S` sells one order at the last shown close, `C` closes the position, `L` and `T`
/// place a limit or stop order at the price under the pointer, `X` cancels the pending orders.
///
/// Market fills happen at the price the user sees, on the last bar shown: with the replay cursor
/// holding back the future, this is all a human can act on. Pending orders wait for bars shown
/// after they were placed, and fill against them as the replay reveals them. On a live chart outside a replay
/// they wait for the trades that come after they were placed.
#[derive(Default)]
pub struct TradingHandler {
    /// The newest bar that had a successor when last looked at: bars up to it are not looked at again.
    /// (The newest bar itself is looked at on every event, as a live one keeps changing.)
    checked: Option<Time>,
    /// The newest bar when last looked at, and how many of its trades were: when it is looked at again (it was
    /// forming, or it is the last one), only its trades after those can fill, as the earlier ones came before
    /// what was placed or filled since.
    newest: Option<(Time, usize)>,
    replaying: bool,
    /// On a live chart, the newest trade looked at and how many trades of its time were: the trades after those
    /// are the ones to look at next.
    tape: Option<(Time, usize)>,
    /// Bars complete when last looked at: fewer means the replay started over.
    complete: usize,
}

impl TradingHandler {
    /// Lets the pending orders meet the bars revealed since the last event.
    fn match_orders(&mut self, ctx: &mut EventCtx<'_>) -> bool {
        // A replay starting or ending is a new session: what was pending belongs to the other one.
        let replaying = ctx.cursor.is_replaying();
        let len = ctx.base.len();
        let complete = len - usize::from(ctx.cursor.forming() && len > 0);
        let restarted = replaying != self.replaying || complete < self.complete;
        (self.replaying, self.complete) = (replaying, complete);
        let live = !replaying && live(ctx.res);
        let Some(account) = ctx.res.get_mut::<Account>() else {
            return false;
        };
        if restarted {
            account.cancel_all();
            (self.checked, self.newest, self.tape) = (None, None, None);
        }
        if live {
            // Each trade is looked at once, against the orders placed before it.
            let times = ctx.trades.time();
            let start = self
                .tape
                .map_or(0, |(t, seen)| times.partition_point(|&x| x < t) + seen)
                .min(times.len());
            self.tape = times
                .last()
                .map(|&t| (t, times.len() - times.partition_point(|&x| x < t)));
            if account.orders().is_empty() {
                return false;
            }
            return account.match_live(ctx.trades.slice(start..times.len()), ctx.base.time()) > 0;
        }
        // Orders meet the bars as they came, whatever timeframe the chart shows, trade by trade where the tape is.
        let time = ctx.base.time();
        let first = self
            .checked
            .map_or(len.saturating_sub(1), |t| time.partition_point(|&x| x <= t));
        let mut changed = false;
        if !account.orders().is_empty() {
            for (i, bar) in (first..len).map_while(|i| Some((i, ctx.base.get(i)?))) {
                let trades = bar_trades(ctx.trades, time, i);
                let filled = match self.newest {
                    Some((t, done)) if t == bar.time => {
                        account.match_trades(bar.time, trades.slice(done.min(trades.len())..trades.len()))
                    }
                    _ => account.match_tape(bar.time, bar.open, bar.high, bar.low, trades),
                };
                changed |= filled > 0;
            }
        }
        self.checked = len.checked_sub(2).map(|i| time[i]);
        self.newest = len
            .checked_sub(1)
            .map(|i| (time[i], bar_trades(ctx.trades, time, i).len()));
        changed
    }

    fn place(&self, kind: OrderKind, ctx: &mut EventCtx<'_>) -> Response {
        // Only the main pane shows the price the orders are in.
        let price = ctx.pointer_price().filter(|_| ctx.pointer_pane() == Some(0));
        let (Some(&newest), Some(&close), Some(price)) = (ctx.base.time().last(), ctx.base.close().last(), price)
        else {
            return Response::IGNORED;
        };
        let after = order_after(newest, ctx.cursor.forming(), ctx.cursor.is_replaying(), ctx.res);
        let Some(account) = ctx.res.get_mut::<Account>() else {
            return Response::IGNORED;
        };
        let below = price < close;
        let buy = match kind {
            OrderKind::Limit => below,
            OrderKind::Stop => !below,
        };
        let qty = if buy { account.order_size } else { -account.order_size };
        // An order against the open position only closes it: that is what a stop loss and a take profit are.
        let reduce_only = account.position() * qty < 0.0;
        account.place(Order {
            kind,
            qty,
            price,
            after,
            reduce_only,
            exits: Exits::default(),
            break_even: None,
        });
        Response::REDRAW
    }
}

impl Handler for TradingHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let matched = self.match_orders(ctx);
        let mut response = if matched { Response::REDRAW } else { Response::IGNORED };
        let Event::Command(command) = *event else {
            return response;
        };
        // Trades are made in a replay or on a live chart: elsewhere no bars or trades come after them, and every bar
        // is in view.
        let trades = matches!(
            command,
            commands::LIMIT_ORDER | commands::STOP_ORDER | commands::BUY | commands::SELL | commands::CLOSE_POSITION
        );
        if trades && !(ctx.cursor.is_replaying() || live(ctx.res)) {
            return response;
        }
        let handled = match command {
            commands::LIMIT_ORDER => self.place(OrderKind::Limit, ctx),
            commands::STOP_ORDER => self.place(OrderKind::Stop, ctx),
            commands::SIZE_UP | commands::SIZE_DOWN => match ctx.res.get_mut::<Account>() {
                Some(account) => {
                    account.order_size = if command == commands::SIZE_UP {
                        size_up(account.order_size)
                    } else {
                        size_down(account.order_size)
                    };
                    Response::REDRAW
                }
                None => Response::IGNORED,
            },
            commands::CANCEL_ORDERS => match ctx.res.get_mut::<Account>() {
                Some(account) if !account.orders().is_empty() => {
                    account.cancel_all();
                    Response::REDRAW
                }
                _ => Response::IGNORED,
            },
            commands::BUY | commands::SELL | commands::CLOSE_POSITION => self.market(command, ctx),
            _ => Response::IGNORED,
        };
        response.redraw |= handled.redraw;
        response.consumed |= handled.consumed;
        response
    }
}

impl TradingHandler {
    fn market(&self, command: &'static str, ctx: &mut EventCtx<'_>) -> Response {
        let (Some(&time), Some(&price)) = (ctx.base.time().last(), ctx.base.close().last()) else {
            return Response::IGNORED;
        };
        let Some(account) = ctx.res.get_mut::<Account>() else {
            return Response::IGNORED;
        };
        let qty = match command {
            commands::BUY => account.order_size,
            commands::SELL => -account.order_size,
            _ => -account.position(),
        };
        account.market(time, price, qty);
        Response::REDRAW
    }
}

const MARKER_ROWS: usize = 7;

/// Fill markers under (buy) and above (sell) their bars, the average price line of the open position
/// with its label on the price axis, and a profit readout.
pub struct TradingLayer;

impl Layer for TradingLayer {
    /// The price labels of the position and the orders sit on the axis.
    fn clip(&self) -> Clip {
        Clip::Pane
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let Some(account) = ctx.res.get::<Account>() else {
            return;
        };
        let style = ctx.style;
        let (plot_w, plot_h) = ctx.plot;
        let unit = ctx.line();
        let gap = ctx.px(3.0);
        let half_marker = (MARKER_ROWS as f32 * unit).max(unit);

        for fill in account.fills() {
            // The marker sits on the bar that holds the fill: on a higher timeframe that is a period, not a bar.
            let Some(i) = ctx
                .view
                .time()
                .partition_point(|&x| x <= fill.time)
                .checked_sub(1)
                .filter(|i| ctx.visible.contains(i))
            else {
                continue;
            };
            let x = ctx.time.x_of(i as f64 + 0.5) as f32;
            if x < half_marker || x > plot_w - half_marker {
                continue;
            }
            let center = (x / unit).round() * unit;
            let (color, up) = if fill.qty > 0.0 {
                (style.candle.up, true)
            } else {
                (style.candle.down, false)
            };
            if up {
                let top = ctx.price.y_of(ctx.view.low()[i]) as f32 + gap;
                for row in 0..MARKER_ROWS {
                    let w = (2 * row + 1) as f32 * unit;
                    ctx.scene
                        .fill(center - w / 2.0, top + row as f32 * unit, w, unit, color);
                }
            } else {
                let bottom = ctx.price.y_of(ctx.view.high()[i]) as f32 - gap;
                for row in 0..MARKER_ROWS {
                    let w = (2 * row + 1) as f32 * unit;
                    ctx.scene
                        .fill(center - w / 2.0, bottom - (row + 1) as f32 * unit, w, unit, color);
                }
            }
        }

        let font_size = ctx.px(style.font_size);
        let label = |ctx: &mut DrawCtx<'_>, y: f32, price: f64, color: Color| {
            let label_h = ctx.px(super::crosshair::PRICE_LABEL_HEIGHT);
            super::axes::axis_tag(ctx, (y - label_h / 2.0, y + label_h / 2.0), color);
            let plot_w = ctx.plot.0;
            let (x, size, text_color) = (plot_w + ctx.px(style.label_padding), font_size, color.ink());
            let text = super::drawings::price_text(price, price);
            ctx.scene.text(x, y, size, text_color, Align::Left, text);
        };
        if let Some(average) = account.average_price() {
            let y = ctx.price.y_of(average) as f32;
            if (0.0..plot_h).contains(&y) {
                ctx.scene.line((0.0, y), (plot_w, y), unit, style.accent);
                label(ctx, y, average, style.accent);
            }
        }
        for order in account.orders() {
            // An exit is coloured by what it does whatever the side, a take profit up and a stop loss down; an
            // entry by its side.
            let color = match (order.reduce_only, order.kind) {
                (true, OrderKind::Limit) => style.candle.up,
                (true, OrderKind::Stop) => style.candle.down,
                _ if order.qty > 0.0 => style.candle.up,
                _ => style.candle.down,
            };
            // Where the stop moves to the entry: dotted, fainter than an order, taken and dragged like one.
            if let Some(trigger) = order.break_even {
                let y = ctx.price.y_of(trigger) as f32;
                if (0.0..plot_h).contains(&y) {
                    let faint = Color { a: 170, ..color };
                    let (dot, gap) = (ctx.px(2.0), ctx.px(4.0));
                    let mut x = 0.0;
                    while x < plot_w {
                        ctx.scene.line((x, y), ((x + dot).min(plot_w), y), unit, faint);
                        x += dot + gap;
                    }
                    let (x, text_y) = (plot_w - ctx.px(8.0), y - font_size * 0.8);
                    ctx.scene.text(x, text_y, font_size, faint, Align::Right, "Break-even");
                    label(ctx, y, trigger, faint);
                }
            }
            let y = ctx.price.y_of(order.price) as f32;
            if !(0.0..plot_h).contains(&y) {
                continue;
            }
            let (dash, gap) = (ctx.px(6.0), ctx.px(4.0));
            let mut x = 0.0;
            while x < plot_w {
                ctx.scene.line((x, y), ((x + dash).min(plot_w), y), unit, color);
                x += dash + gap;
            }
            // An order that only closes is an exit: named as the person placed it.
            let side = if order.qty > 0.0 { "Buy" } else { "Sell" };
            let name = match (order.reduce_only, order.kind) {
                (true, OrderKind::Limit) => "Take profit".to_string(),
                (true, OrderKind::Stop) => "Stop loss".to_string(),
                (false, OrderKind::Limit) => format!("{side} limit"),
                (false, OrderKind::Stop) => format!("{side} stop"),
            };
            let text = format!("{name} {}", order.qty.abs());
            ctx.scene.text(
                plot_w - ctx.px(8.0),
                y - font_size * 0.8,
                font_size,
                color,
                Align::Right,
                text,
            );
            label(ctx, y, order.price, color);
        }

        if account.fills().is_empty() || ctx.res.get::<super::NoReadouts>().is_some() {
            return;
        }
        let pnl = ctx
            .view
            .close()
            .last()
            .map_or(account.realized() - account.fees(), |&c| account.net(c));
        let color: Color = if pnl >= 0.0 { style.candle.up } else { style.candle.down };
        let position = match account.average_price() {
            Some(avg) => format!("Position {:+} @ {avg:.2}", account.position()),
            None => "Flat".to_string(),
        };
        let fees = if account.fees() > 0.0 {
            format!("   Fees {:.2}", account.fees())
        } else {
            String::new()
        };
        let (pnl, realized) = (
            crate::ticks::signed(pnl, 2),
            crate::ticks::signed(account.realized(), 2),
        );
        let text = format!("{position}   P&L {pnl}   Realized {realized}{fees}");
        ctx.scene
            .text(ctx.px(12.0), ctx.px(34.0), font_size, color, Align::Left, text);
        let stats = account.stats();
        if stats.closed > 0 {
            let win = stats
                .win_rate()
                .map_or(String::new(), |w| format!("   Win {:.0}%", w * 100.0));
            let factor = stats
                .profit_factor()
                .map_or(String::new(), |f| format!("   Profit factor {f:.2}"));
            let text = format!(
                "Trades {}{win}{factor}   Max drawdown {:.2}",
                stats.closed, stats.max_drawdown
            );
            ctx.scene
                .text(ctx.px(12.0), ctx.px(52.0), font_size, style.hud_text, Align::Left, text);
        }
    }

    /// The position line is always on screen, even when the price runs away from where it was entered.
    fn price_extent(&self, view: CandleView<'_>, _visible: Range<usize>, res: &Resources) -> Option<(f64, f64)> {
        if view.is_empty() {
            return None;
        }
        res.get::<Account>()?.average_price().map(|average| (average, average))
    }
}

/// How close to an order line (in logical pixels) the pointer must be to take it.
const GRAB: f32 = 6.0;

/// Drag a pending order's line to another price. The order keeps its kind and size; a limit dragged across the
/// market simply becomes one that fills on the next bar, as it would have if placed there. A stop loss's
/// break-even line drags the same way.
#[derive(Default)]
pub struct OrderDragHandler {
    /// The order taken, and whether by its break-even.
    grabbed: Option<(usize, bool)>,
}

impl OrderDragHandler {
    /// The order line (or break-even line) under the pointer in the main pane.
    fn line_at(ctx: &EventCtx<'_>) -> Option<(usize, bool)> {
        let (Some((_, y)), Some(Some(price)), Some(main)) =
            (ctx.pointer.position, ctx.prices.first(), ctx.panes.first())
        else {
            return None;
        };
        if ctx.pointer_pane() != Some(0) {
            return None;
        }
        let account = ctx.res.get::<Account>()?;
        let reach = GRAB * ctx.scale;
        let lines = account.orders().iter().enumerate().flat_map(|(i, o)| {
            let trigger = o.break_even.map(|t| ((i, true), t));
            std::iter::once(((i, false), o.price)).chain(trigger)
        });
        lines
            .map(|(at, p)| (at, (main.top + price.y_of(p) as f32 - y).abs()))
            .filter(|&(_, d)| d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(at, _)| at)
    }
}

impl Handler for OrderDragHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if matches!(event, Event::PointerMoved { .. })
            && (self.grabbed.is_some() || (!ctx.pointer.pressed && Self::line_at(ctx).is_some()))
        {
            *ctx.shape = crate::PointerShape::ResizeVertical;
        }
        match event {
            Event::PointerDown => {
                self.grabbed = Self::line_at(ctx);
                if self.grabbed.is_some() {
                    *ctx.shape = crate::PointerShape::ResizeVertical;
                    Response::CONSUMED_REDRAW
                } else {
                    Response::IGNORED
                }
            }
            Event::PointerMoved { y, .. } if self.grabbed.is_some() => {
                let (Some((index, trigger)), Some(Some(scale)), Some(main)) =
                    (self.grabbed, ctx.prices.first(), ctx.panes.first())
                else {
                    return Response::IGNORED;
                };
                let price = scale.price_of((*y - main.top) as f64);
                let moved = ctx.res.get_mut::<Account>().is_some_and(|a| match trigger {
                    true => a.set_break_even(index, Some(price)),
                    false => a.move_order(index, price),
                });
                Response {
                    redraw: moved,
                    consumed: true,
                    ..Response::IGNORED
                }
            }
            Event::PointerUp | Event::PointerLeft if self.grabbed.take().is_some() => Response::CONSUMED_REDRAW,
            _ => Response::IGNORED,
        }
    }
}

pub struct TradingPlugin;

fn replaying(res: &Resources) -> bool {
    res.get::<super::Replay>().is_some_and(|r| r.active)
}

impl Plugin for TradingPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(Account::default());
        // The trade buttons are there while trades can be made: in a replay, or on a live chart.
        chart.add_button(
            ToolButton::new(commands::BUY, Label::Fixed("Buy"))
                .kind(ButtonKind::Buy)
                .visible_when(can_trade),
        );
        chart.add_button(
            ToolButton::new(commands::SELL, Label::Fixed("Sell"))
                .kind(ButtonKind::Sell)
                .visible_when(can_trade),
        );
        chart.add_button(
            ToolButton::new(commands::CLOSE_POSITION, Label::Fixed("Close"))
                .visible_when(|res| can_trade(res) && res.get::<Account>().is_some_and(|a| a.position() != 0.0)),
        );
        chart.bind(Key::Char('b'), commands::BUY);
        chart.bind(Key::Char('s'), commands::SELL);
        // Clicking the size steps it up; the keys go both ways.
        chart.add_button(
            ToolButton::new(
                commands::SIZE_UP,
                Label::Dynamic(|res| format!("Size {}", res.get::<Account>().map_or(1.0, |a| a.order_size))),
            )
            .visible_when(can_trade),
        );
        chart.bind(Key::Char(']'), commands::SIZE_UP);
        chart.bind(Key::Char('['), commands::SIZE_DOWN);
        chart.bind(Key::Char('c'), commands::CLOSE_POSITION);
        chart.bind(Key::Char('l'), commands::LIMIT_ORDER);
        chart.bind(Key::Char('t'), commands::STOP_ORDER);
        chart.bind(Key::Char('x'), commands::CANCEL_ORDERS);
        chart.add_handler("order-drag", order::ORDER_DRAG_HANDLER, OrderDragHandler::default());
        chart.add_handler(names::TRADING, order::TRADING_HANDLER, TradingHandler::default());
        chart.add_layer(names::TRADING, order::TRADES, TradingLayer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: i64) -> Time {
        Time(n)
    }

    #[test]
    fn long_round_trip() {
        let mut a = Account::default();
        a.fill(t(1), 100.0, 2.0);
        assert_eq!((a.position(), a.average_price()), (2.0, Some(100.0)));
        a.fill(t(2), 110.0, -1.0);
        assert_eq!(a.position(), 1.0);
        assert_eq!(a.realized(), 10.0);
        assert_eq!(a.unrealized(120.0), 20.0);
        a.fill(t(3), 90.0, -1.0);
        assert_eq!((a.position(), a.average_price(), a.realized()), (0.0, None, 0.0));
        assert_eq!(a.fills().len(), 3);
    }

    #[test]
    fn a_trade_pays_the_fees_of_its_part_of_the_position() {
        let mut a = Account {
            fee_rate: 0.01,
            ..Account::default()
        };
        a.fill(t(1), 100.0, 2.0); // fee 2
        a.fill(t(2), 101.5, -1.0); // +1.5, less half the entry fee and its own 1.015: a loss
        a.fill(t(3), 100.0, -2.0); // closes the other half at 0, flips short with half of its fee of 2
        a.fill(t(4), 90.0, 1.0); // +10, less 1 of entry and 0.9 of exit
        let results = Account::results(a.fills());
        let expected = [None, Some(-0.515), Some(-2.0), Some(8.1)];
        for (got, want) in results.iter().zip(expected) {
            assert_eq!(got.is_some(), want.is_some());
            assert!((got.unwrap_or(0.0) - want.unwrap_or(0.0)).abs() < 1e-9, "{results:?}");
        }
        let s = a.stats();
        assert_eq!((s.closed, s.wins), (3, 1));
        // Together the trades come to what the account made.
        let total = s.gross_profit - s.gross_loss;
        assert!((total - (a.realized() - a.fees())).abs() < 1e-9);
    }

    #[test]
    fn an_account_restored_from_its_history_is_the_same() {
        let mut a = Account {
            fee_rate: 0.001,
            order_size: 2.0,
            ..Account::default()
        };
        a.fill(t(1), 100.0, 2.0);
        a.fill(t(2), 110.0, -3.0);
        a.protect(
            t(2),
            -1.0,
            Exits {
                take_profit: Some(90.0),
                stop_loss: Some(120.0),
                break_even: None,
            },
        );
        // A later fee rate does not change what was paid.
        let b = Account::restore(a.order_size, 0.5, a.fills(), a.orders());
        assert_eq!((b.position(), b.average_price()), (a.position(), a.average_price()));
        assert_eq!((b.realized(), b.fees(), b.stats()), (a.realized(), a.fees(), a.stats()));
        assert_eq!((b.fills(), b.orders(), b.order_size), (a.fills(), a.orders(), 2.0));
    }

    #[test]
    fn short_profits_when_price_falls() {
        let mut a = Account::default();
        a.fill(t(1), 100.0, -1.0);
        assert_eq!(a.unrealized(90.0), 10.0);
        a.fill(t(2), 90.0, 1.0);
        assert_eq!(a.realized(), 10.0);
        assert_eq!(a.position(), 0.0);
    }

    #[test]
    fn adding_to_a_position_averages_the_price() {
        let mut a = Account::default();
        a.fill(t(1), 100.0, 1.0);
        a.fill(t(2), 110.0, 3.0);
        assert_eq!(a.average_price(), Some(107.5));
    }

    #[test]
    fn flipping_through_zero_realizes_and_reopens_at_the_new_price() {
        let mut a = Account::default();
        a.fill(t(1), 100.0, 1.0);
        a.fill(t(2), 110.0, -3.0);
        assert_eq!(a.realized(), 10.0);
        assert_eq!(a.position(), -2.0);
        assert_eq!(a.average_price(), Some(110.0));
        a.fill(t(3), 100.0, 2.0);
        assert_eq!(a.realized(), 30.0);
        assert_eq!(a.position(), 0.0);
    }

    #[test]
    fn order_sizes_step_in_ones_twos_and_fives() {
        let ups: Vec<f64> = std::iter::successors(Some(0.1), |&s| Some(size_up(s)))
            .take(8)
            .collect();
        assert_eq!(ups, [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0]);
        let downs: Vec<f64> = std::iter::successors(Some(10.0), |&s| Some(size_down(s)))
            .take(6)
            .collect();
        assert_eq!(downs, [10.0, 5.0, 2.0, 1.0, 0.5, 0.2]);
        assert_eq!(size_up(3.0), 5.0, "off the ladder goes to the next rung");
        assert_eq!(size_down(3.0), 2.0);
        assert_eq!(size_up(0.0), 1.0);
        assert_eq!(size_down(f64::NAN), 1.0);
        assert!(size_down(1e-300) > 0.0, "never down to nothing");
        assert!(size_up(1e300).is_finite());
    }

    #[test]
    fn nonsense_fills_are_ignored() {
        let mut a = Account::default();
        a.fill(t(1), f64::NAN, 1.0);
        a.fill(t(1), 100.0, 0.0);
        assert!(a.fills().is_empty());
    }

    #[test]
    fn market_and_stop_orders_slip_against_the_order_and_limits_fill_at_their_price() {
        let mut a = Account {
            slippage: 0.001,
            ..Account::default()
        };
        a.market(t(1), 100.0, 1.0);
        a.market(t(1), 100.0, -1.0);
        a.place(order(OrderKind::Stop, 1.0, 105.0));
        a.place(order(OrderKind::Limit, -1.0, 106.0));
        assert_eq!(a.match_bar(t(2), 104.0, 107.0, 103.0), 2);
        let prices: Vec<f64> = a.fills().iter().map(|f| f.price).collect();
        let want = [100.1, 99.9, 105.105, 106.0];
        assert!(prices.iter().zip(want).all(|(p, w)| (p - w).abs() < 1e-9), "{prices:?}");
        assert_eq!(a.empty_like().slippage, 0.001, "a strategy's run slips the same");
    }

    fn order(kind: OrderKind, qty: f64, price: f64) -> Order {
        Order {
            kind,
            qty,
            price,
            after: t(0),
            reduce_only: false,
            exits: Exits::default(),
            break_even: None,
        }
    }

    #[test]
    fn limits_fill_at_their_price_or_better() {
        let mut a = Account::default();
        a.place(order(OrderKind::Limit, 1.0, 100.0));
        assert_eq!(
            a.match_bar(t(1), 105.0, 110.0, 101.0),
            0,
            "the bar stays above a buy limit"
        );
        assert_eq!(a.match_bar(t(2), 105.0, 106.0, 99.0), 1);
        assert_eq!(a.fills()[0].price, 100.0);
        a.place(order(OrderKind::Limit, 1.0, 100.0));
        a.match_bar(t(3), 95.0, 96.0, 94.0); // gaps below the limit: the open is the better price
        assert_eq!(a.fills()[1].price, 95.0);
        a.place(order(OrderKind::Limit, -1.0, 100.0));
        a.match_bar(t(4), 103.0, 104.0, 102.0);
        assert_eq!(a.fills()[2].price, 103.0, "a sell limit gets the higher open");
    }

    #[test]
    fn stops_fill_at_their_price_or_worse() {
        let mut a = Account::default();
        a.place(order(OrderKind::Stop, 1.0, 100.0));
        assert_eq!(a.match_bar(t(1), 90.0, 99.0, 89.0), 0);
        a.match_bar(t(2), 98.0, 101.0, 97.0);
        assert_eq!(a.fills()[0].price, 100.0);
        a.place(order(OrderKind::Stop, -1.0, 100.0));
        a.match_bar(t(3), 90.0, 92.0, 88.0); // gaps through the stop: filled at the open
        assert_eq!(a.fills()[1].price, 90.0);
        assert_eq!(a.position(), 0.0);
    }

    #[test]
    fn an_order_never_fills_on_the_bar_it_was_placed_on_or_before_it() {
        let mut a = Account::default();
        a.place(Order {
            kind: OrderKind::Limit,
            qty: 1.0,
            price: 100.0,
            after: t(5),
            reduce_only: false,
            exits: Exits::default(),
            break_even: None,
        });
        assert_eq!(a.match_bar(t(5), 100.0, 101.0, 99.0), 0);
        assert_eq!(a.match_bar(t(4), 100.0, 101.0, 99.0), 0);
        assert_eq!(a.match_bar(t(6), 100.0, 101.0, 99.0), 1);
    }

    #[test]
    fn a_reduce_only_order_never_opens_or_flips_and_goes_when_nothing_is_left_to_close() {
        let mut a = Account::default();
        a.fill(t(0), 100.0, 1.0);
        let reduce = |kind, price| Order {
            reduce_only: true,
            ..order(kind, -1.0, price)
        };
        a.place(reduce(OrderKind::Stop, 95.0)); // stop loss
        a.place(reduce(OrderKind::Limit, 110.0)); // take profit
        assert_eq!(a.match_bar(t(1), 97.0, 99.0, 96.0), 0);
        assert_eq!(a.match_bar(t(2), 109.0, 111.0, 108.0), 1, "the take profit fills");
        assert_eq!(a.position(), 0.0);
        // The stop loss is dropped with the next bar, even one that reaches it: it would have opened a short.
        assert_eq!(a.match_bar(t(3), 96.0, 97.0, 90.0), 0);
        assert!(a.orders().is_empty());
        assert_eq!(a.position(), 0.0);
        // Bigger than the position: only what is open is closed.
        a.fill(t(4), 100.0, 1.0);
        a.place(Order {
            reduce_only: true,
            ..order(OrderKind::Limit, -5.0, 105.0)
        });
        a.match_bar(t(5), 104.0, 106.0, 103.0);
        assert_eq!(a.fills().last().unwrap().qty, -1.0);
        assert_eq!(a.position(), 0.0);
    }

    #[test]
    fn exits_are_placed_when_the_entry_fills_and_work_from_the_next_bar() {
        let mut a = Account::default();
        let exits = Exits {
            take_profit: Some(110.0),
            stop_loss: Some(95.0),
            break_even: None,
        };
        a.place(Order {
            exits,
            ..order(OrderKind::Limit, 2.0, 100.0)
        });
        // The entry fills on a bar that also reaches the stop loss: the stop loss waits for the next bar.
        assert_eq!(a.match_bar(t(1), 101.0, 102.0, 94.0), 1);
        assert_eq!(a.position(), 2.0);
        let orders = a.orders();
        assert_eq!(orders.len(), 2);
        assert!(orders.iter().all(|o| o.reduce_only && o.qty == -2.0 && o.after == t(1)));
        assert_eq!(a.match_bar(t(2), 96.0, 97.0, 94.0), 1, "the stop loss");
        assert_eq!(a.position(), 0.0);
        assert_eq!(a.match_bar(t(3), 100.0, 111.0, 99.0), 0, "the take profit went with it");
        assert!(a.orders().is_empty());

        // A market entry is protected the same way.
        a.fill(t(4), 100.0, -1.0);
        let short_exits = Exits {
            take_profit: Some(90.0),
            stop_loss: Some(105.0),
            break_even: None,
        };
        a.protect(t(4), -1.0, short_exits);
        a.match_bar(t(5), 100.0, 101.0, 89.0);
        assert_eq!(a.fills().last().unwrap().price, 90.0, "a short takes profit below");
        assert_eq!(a.position(), 0.0);
    }

    fn tape(trades: &[(i64, f64)]) -> crate::Trades {
        let mut tape = crate::Trades::new();
        for &(time, price) in trades {
            let side = crate::Side::Buy;
            tape.push(crate::Trade {
                time: t(time),
                price,
                qty: 1.0,
                side,
            })
            .unwrap();
        }
        tape
    }

    #[test]
    fn the_tape_says_whether_the_stop_or_the_target_of_a_bar_came_first() {
        let mut long = Account::default();
        long.fill(t(0), 100.0, 1.0);
        let exits = Exits {
            take_profit: Some(110.0),
            stop_loss: Some(95.0),
            break_even: None,
        };
        long.protect(t(0), 1.0, exits);
        let bar = (t(10), 100.0, 111.0, 94.0);
        let last = |a: &Account| (a.fills().last().unwrap().price, a.position(), a.orders().len());

        let mut blind = long.clone();
        blind.match_bar(bar.0, bar.1, bar.2, bar.3);
        assert_eq!(
            last(&blind),
            (95.0, 0.0, 0),
            "without the tape the worse: the stop, though the target was placed first"
        );

        let mut down_first = long.clone();
        let trades = tape(&[(11, 100.0), (12, 97.0), (13, 95.0), (14, 94.0), (15, 111.0)]);
        assert_eq!(down_first.match_tape(bar.0, bar.1, bar.2, bar.3, trades.view()), 1);
        assert_eq!(last(&down_first), (95.0, 0.0, 0));

        let mut up_first = long.clone();
        let trades = tape(&[(11, 100.0), (12, 111.0), (13, 94.0)]);
        up_first.match_tape(bar.0, bar.1, bar.2, bar.3, trades.view());
        assert_eq!(last(&up_first), (110.0, 0.0, 0), "a limit fills at its own price");

        let mut gapped = long;
        let trades = tape(&[(11, 100.0), (12, 96.0), (13, 93.0)]);
        gapped.match_tape(t(10), 100.0, 100.0, 93.0, trades.view());
        assert_eq!(last(&gapped), (93.0, 0.0, 0), "a stop at the trade that set it off");
    }

    #[test]
    fn a_stop_moves_to_the_entry_once_the_price_reaches_its_break_even() {
        let protected = |qty: f64, stop: f64, trigger: f64| {
            let mut a = Account::default();
            a.fill(t(0), 100.0, qty);
            let exits = Exits {
                take_profit: None,
                stop_loss: Some(stop),
                break_even: Some(trigger),
            };
            a.protect(t(0), qty, exits);
            a
        };
        // On bars, after the bar that reached it: the bar may have come back to the entry first.
        let mut long = protected(1.0, 95.0, 105.0);
        long.match_bar(t(1), 101.0, 104.0, 100.0);
        assert_eq!(long.orders()[0].price, 95.0, "not reached");
        long.match_bar(t(2), 101.0, 106.0, 99.0);
        assert_eq!((long.orders()[0].price, long.orders()[0].break_even), (100.0, None));
        assert_eq!(long.position(), 1.0, "not out on the bar that moved it");
        long.match_bar(t(3), 101.0, 102.0, 99.5);
        assert_eq!((long.position(), long.fills()[1].price), (0.0, 100.0));

        // On the tape, at the trade that reached it: later trades of the bar reach the moved stop.
        let mut long = protected(1.0, 95.0, 105.0);
        let trades = tape(&[(10, 101.0), (11, 105.5), (12, 99.0)]);
        long.match_tape(t(1), 101.0, 105.5, 99.0, trades.view());
        assert_eq!((long.position(), long.fills()[1].price), (0.0, 99.0));

        // A short's stop is above: a fall to the trigger moves it down to the entry.
        let mut short = protected(-1.0, 105.0, 95.0);
        short.match_bar(t(1), 99.0, 100.0, 94.0);
        assert_eq!(short.orders()[0].price, 100.0);

        // Only a stop loss has one.
        let mut a = protected(1.0, 95.0, 105.0);
        a.protect(
            t(0),
            1.0,
            Exits {
                take_profit: Some(110.0),
                ..Exits::default()
            },
        );
        assert!(!a.set_break_even(1, Some(103.0)), "a take profit");
        assert!(a.set_break_even(0, Some(103.0)) && a.orders()[0].break_even == Some(103.0));
        assert!(!a.set_break_even(0, Some(f64::NAN)) && a.set_break_even(0, None));
    }

    #[test]
    fn with_the_tape_an_entry_s_exits_work_from_the_next_trade_of_its_bar() {
        let entry = Order {
            exits: Exits {
                take_profit: Some(105.0),
                stop_loss: Some(99.0),
                break_even: None,
            },
            ..order(OrderKind::Limit, 1.0, 100.0)
        };
        let mut a = Account::default();
        a.place(entry);
        let trades = tape(&[(11, 102.0), (12, 100.0), (13, 104.0), (14, 106.0)]);
        assert_eq!(a.match_tape(t(10), 102.0, 106.0, 100.0, trades.view()), 2);
        let prices: Vec<f64> = a.fills().iter().map(|f| f.price).collect();
        assert_eq!((prices, a.position()), (vec![100.0, 105.0], 0.0));
        assert!(a.fills().iter().all(|f| f.time == t(10)), "fills keep the bar's time");

        // The trade that fills the entry does not also set off its stop.
        let mut a = Account::default();
        a.place(entry);
        let trades = tape(&[(11, 101.0), (12, 98.0), (13, 101.0)]);
        assert_eq!(a.match_tape(t(10), 101.0, 101.0, 98.0, trades.view()), 1);
        assert_eq!((a.position(), a.orders().len()), (1.0, 2));
        // Its next trade does.
        let trades = tape(&[(21, 98.5)]);
        assert_eq!(a.match_tape(t(20), 98.5, 98.5, 98.5, trades.view()), 1);
        assert_eq!((a.fills()[1].price, a.position()), (98.5, 0.0));
    }

    #[test]
    fn nonsense_orders_are_not_placed() {
        let mut a = Account::default();
        a.place(order(OrderKind::Limit, 0.0, 100.0));
        a.place(order(OrderKind::Limit, 1.0, f64::NAN));
        a.place(order(OrderKind::Limit, 1.0, -5.0));
        assert!(a.orders().is_empty());
    }

    #[test]
    fn fees_are_charged_on_every_fill_and_reduce_the_net_result() {
        let mut a = Account {
            fee_rate: 0.001,
            ..Account::default()
        };
        a.fill(t(1), 100.0, 2.0); // 0.2
        a.fill(t(2), 110.0, -2.0); // 0.22
        assert!((a.fees() - 0.42).abs() < 1e-12);
        assert_eq!(a.realized(), 20.0);
        assert!((a.net(0.0) - 19.58).abs() < 1e-12);
        assert!((a.fills()[1].fee - 0.22).abs() < 1e-12);
    }

    #[test]
    fn stats_count_closing_fills_and_measure_drawdown() {
        let mut a = Account::default();
        assert_eq!(a.stats(), Stats::default());
        a.fill(t(1), 100.0, 1.0); // opens
        a.fill(t(2), 110.0, -1.0); // +10
        a.fill(t(3), 110.0, -1.0); // opens a short
        a.fill(t(4), 118.0, 1.0); // -8
        a.fill(t(5), 100.0, 1.0);
        a.fill(t(6), 95.0, -1.0); // -5
        let s = a.stats();
        assert_eq!((s.closed, s.wins), (3, 1));
        assert_eq!((s.gross_profit, s.gross_loss), (10.0, 13.0));
        assert_eq!(s.max_drawdown, 13.0, "from the peak of +10 down to -3");
        assert_eq!(s.max_drawdown_share, 13.0 / 10_010.0, "of the balance at the peak");
        assert_eq!(s.win_rate(), Some(1.0 / 3.0));
        assert_eq!(s.profit_factor(), Some(10.0 / 13.0));
        assert_eq!(
            (s.win_streak, s.loss_streak),
            (1, 2),
            "the short and the last long lost in a row"
        );
        assert_eq!(s.expectancy(), Some(-1.0));
        assert_eq!((s.average_win(), s.average_loss()), (Some(10.0), Some(6.5)));
        assert_eq!(Stats::default().win_rate(), None);
        assert_eq!(
            Stats {
                closed: 1,
                wins: 1,
                gross_profit: 5.0,
                ..Stats::default()
            }
            .profit_factor(),
            None
        );
    }

    fn bars(closes: &[f64]) -> crate::Candles {
        let mut c = crate::Candles::new();
        for (i, &p) in closes.iter().enumerate() {
            c.push(crate::Candle {
                time: t(i as i64),
                open: p,
                high: p,
                low: p,
                close: p,
                volume: 0.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn other_stops_and_targets_are_tried_on_the_bars_after_each_entry() {
        let candles = bars(&[100.0, 99.4, 102.1, 96.0, 97.0, 100.5]);
        let fill = |time, price, qty| Fill {
            time: t(time),
            price,
            qty,
            fee: 1.0,
        };
        // A long out at a loss of 3%, turned short there and out 3.5 higher; the long left open does not count.
        let fills = [fill(0, 100.0, 1.0), fill(4, 97.0, -2.0), fill(5, 100.5, 2.0)];
        let tried = exits_tried(&fills, candles.view());
        assert_eq!(tried.trades, 2);
        assert!((tried.as_traded - (-0.03 - 3.5 / 97.0)).abs() < 1e-12, "before fees");
        let total = |stop, target| {
            tried
                .tried
                .iter()
                .find(|e| e.stop == stop && e.target == target)
                .unwrap()
                .total
        };
        // The 0.5% stop is reached a bar before the 2% target; the 1% stop is not.
        assert!((total(0.005, 0.02) - -0.01).abs() < 1e-12);
        assert!((total(0.01, 0.02) - 0.01).abs() < 1e-12);
        // Neither reached: out at the last close.
        assert!((total(0.05, 0.05) - (0.005 - 3.5 / 97.0)).abs() < 1e-12);
        let best = tried.tried[0];
        assert_eq!((best.stop, best.target), (0.0075, 0.02));
        assert!((best.total - 0.0125).abs() < 1e-12);
        assert_eq!(tried.tried.len(), EXIT_SHARES.len() * EXIT_SHARES.len());
        // Nothing past the bars given, as at a replay's cursor: by 99.4 neither the 1% stop nor the 2% target was
        // reached, so both trips go out at that last close.
        let seen = exits_tried(&fills, candles.view().until(t(1)));
        let e = seen.tried.iter().find(|e| e.stop == 0.01 && e.target == 0.02).unwrap();
        assert!((e.total - (-0.006 - (99.4 / 97.0 - 1.0))).abs() < 1e-12, "{}", e.total);
    }

    #[test]
    fn the_equity_curve_marks_the_position_to_every_close() {
        let candles = bars(&[100.0, 101.0, 103.0, 102.0, 90.0]);
        let mut a = Account {
            fee_rate: 0.01,
            ..Account::default()
        };
        a.fill(t(0), 100.0, 1.0); // fee 1.00
        a.fill(t(3), 102.0, -1.0); // fee 1.02, +2
        let curve = a.equity_curve(candles.view(), 0..5);
        let values: Vec<f64> = curve.iter().map(|p| p.1).collect();
        let want = [-1.0, 0.0, 2.0, -0.02, -0.02];
        for (got, want) in values.iter().zip(want) {
            assert!((got - want).abs() < 1e-9, "{values:?}");
        }
        // Only the asked bars, and never past the view.
        assert_eq!(
            a.equity_curve(candles.view(), 3..99)
                .iter()
                .map(|p| p.0)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(a.equity_curve(candles.view().until(t(1)), 0..5).len(), 2);
        // The bars before the range still count: fills made earlier are in the state.
        assert!((a.equity_curve(candles.view(), 2..3)[0].1 - 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_position_line_stays_in_autoscale() {
        use crate::{Candle, Candles};
        let mut candles = Candles::new();
        candles
            .push(Candle {
                time: t(1),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 0.0,
            })
            .unwrap();
        let mut res = Resources::default();
        let layer = TradingLayer;
        assert_eq!(layer.price_extent(candles.view(), 0..1, &res), None, "no account");
        res.insert(Account::default());
        assert_eq!(layer.price_extent(candles.view(), 0..1, &res), None, "flat");
        res.get_mut::<Account>().unwrap().fill(t(1), 42.0, 1.0);
        assert_eq!(layer.price_extent(candles.view(), 0..1, &res), Some((42.0, 42.0)));
        assert_eq!(
            layer.price_extent(Candles::new().view(), 0..0, &res),
            None,
            "nothing shown"
        );
    }
}
