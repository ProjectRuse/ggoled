# Price ticker

Shows prices on the OLED. Turn it on with **Display → Price ticker** in the tray,
or per schedule entry with `display = "Ticker"`.

## Layouts (tray: Ticker layout)

- **One at a time (large):** symbol and today's change on top, the price in
  large digits, and today's price movement as a line along the bottom (the
  dotted line is the previous close). Switches asset every `cycle_secs`.
- **All at once (list):** one asset per row (label, price, change), plus the
  `compact_symbols` sharing one row with just their % change (name above,
  change below). If there's a spare row, the top row shows the time, or what's
  playing (`grid_header`). If the assets don't fit, they page every
  `cycle_secs` and the compact row stays on every page.
- **Scrolling:** all assets on one line, scrolling across the screen.

## Symbols and timing

Edit the `[ticker]` section of the config file (tray: Schedule → Edit config
file), then Schedule → Reload config.

```toml
[ticker]
symbols = ["BTC-USD", "MSTR"]
compact_symbols = ["^GSPC", "^DJI", "^IXIC"]   # list layout: one row, % only (max 4)
grid_header = "Time"  # list layout top row: "Time", "Media" (time when nothing plays), "None"
mode = "Cycle"        # "Cycle", "Grid" or "Scroll"
cycle_secs = 6        # per asset / page
refresh_secs = 10     # minimum 5

[ticker.labels]       # optional display names
"^GSPC" = "S&P"
```

In the one-at-a-time and scrolling layouts, compact symbols are shown like the
others. Content stays clear of the screen edges by the OLED shift amount, so
nothing is cut off when the image moves.

Symbols are Yahoo Finance symbols: stocks and ETFs as-is (`MSTR`, `SPY`),
crypto as pairs (`BTC-USD`, `ETH-USD`), indexes with `^` (`^GSPC`). The
display drops `-USD` and `^`. Look a symbol up on finance.yahoo.com if unsure.

Prices are fetched in a single request for all symbols, only while the ticker is
on screen. In the one-at-a-time layout they're also refreshed a second before
each switch, so every asset appears with a current price. If Yahoo starts
refusing requests, the ticker backs off automatically, and a price more than
two minutes old gets a `?` after its symbol. Today's change is
measured from the previous close (for crypto, roughly since midnight UTC).
Outside regular trading hours, stocks and ETFs show their newest
extended-hours price, tagged **PRE** (pre-market, 4–9:30am ET), **AH**
(after hours, 4–8pm ET) or **OVN** (overnight, 8pm–4am ET), with the change
measured from the last regular-session close, like Yahoo's page. Indexes and
crypto aren't affected. This takes one extra request per minute for all
stocks together, only while the market is closed. It uses Yahoo's quote
endpoint, which needs a session token that ggoled gets the way a browser does
(no account); if that ever stops working, it falls back to pre-market and
after-hours prices from the chart data (no overnight).

The data comes from Yahoo Finance's public (unofficial) chart endpoint, which
needs no API key but could change without notice.
