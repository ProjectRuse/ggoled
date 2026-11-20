# Price ticker

Shows prices on the OLED. Turn it on with **Display → Price ticker** in the tray,
or per schedule entry with `display = "Ticker"`.

## Layouts (tray: Ticker layout)

- **One at a time (large):** symbol and today's change on top, the price in
  large digits, and today's price movement as a line along the bottom (the
  dotted line is the previous close). Switches asset every `cycle_secs`.
- **All at once (list):** up to four assets, one per row; more than four page
  every `cycle_secs`.
- **Scrolling:** all assets on one line, scrolling across the screen.

## Symbols and timing

Edit the `[ticker]` section of the config file (tray: Schedule → Edit config
file), then Schedule → Reload config.

```toml
[ticker]
symbols = ["BTC-USD", "MSTR"]
mode = "Cycle"        # "Cycle", "Grid" or "Scroll"
cycle_secs = 6        # per asset / page
refresh_secs = 60     # minimum 15
```

Symbols are Yahoo Finance symbols: stocks and ETFs as-is (`MSTR`, `SPY`),
crypto as pairs (`BTC-USD`, `ETH-USD`), indexes with `^` (`^GSPC`). The
display drops `-USD` and `^`. Look a symbol up on finance.yahoo.com if unsure.

Prices are only fetched while the ticker is on screen. Today's change is
measured from the previous close (for crypto, roughly since midnight UTC).
Stock prices follow regular trading hours only (no pre/after-market moves),
and some exchanges' data may be delayed.

The data comes from Yahoo Finance's public (unofficial) chart endpoint, which
needs no API key but could change without notice.
