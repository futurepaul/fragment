---
name: trading-agent-finite
description: "Build sourced stock/crypto/macro comparisons and Plotly market charts with yfinance, CCXT and public FRED data. Analysis only; no order execution."
tags: [finance, trading, charts, macro, crypto, stocks, polymarket]
---

## Deployment prerequisites

The deployment may have no operator exchange key or paid research key. Public yfinance/CCXT/FRED reads and this charting workflow need none. Authenticated exchange actions require separately provisioned credentials: say plainly when missing; this skill does not place orders. Use Hermes native web research for current news instead of the deleted Perplexity helper. Optional Hermes stock skills handle simple standalone quotes; use this skill for combined analysis/charts.

Package versions checked 2026-10-09: Plotly 7.1.0, Kaleido 1.5.0, pandas 3.0.6, yfinance 1.7.0, CCXT 4.5.85, Pillow 12.3.0. Kaleido needs a working Chromium executable; use the image's browser via `BROWSER_PATH=/opt/fragment/bin/chromium`.

Fragment names are `<label>--<suffix>`; people are email addresses. Provider handles, wallet addresses and Nostr pubkeys are separate identities. Never interpret them as Fragment names or people.


# Trading Agent Finite

Use this when the user wants market analysis, charting, macro overlays, or trading-oriented research.

The workflow uses this Python stack:
- `plotly` + `kaleido` for chart rendering
- `pandas`
- `yfinance`
- `ccxt`
- `Pillow`

## Environment

Keep these packages in a virtualenv of your own, made once in your home
(your computer keeps it across sleeps), and activate it before running
Python snippets:

```bash
MARKETS_UV="$(cd /opt/hermes && /opt/hermes/.venv/bin/python -c 'from pm._uv import _toolchain; print(_toolchain(realize=False)[0])')"
[ -d ~/.venvs/markets ] || "$MARKETS_UV" --no-config venv ~/.venvs/markets
"$MARKETS_UV" --no-config pip install --python ~/.venvs/markets/bin/python plotly==7.1.0 kaleido==1.5.0 pandas==3.0.6 yfinance==1.7.0 ccxt==4.5.85 Pillow==12.3.0
export BROWSER_PATH=/opt/fragment/bin/chromium
source ~/.venvs/markets/bin/activate
```

## Source Selection

Use the lightest reliable source for the question:

| Question | Best source |
|---|---|
| Quick stock / ETF / crypto OHLCV | `yfinance` |
| Exchange-specific crypto OHLCV or orderbook | `ccxt` |
| Macro series: rates, CPI, GDP, unemployment, yield curve | FRED's public CSV download (no key) |
| Event probabilities / market sentiment / valuation odds | `polymarket-finite` |
| Private company valuation or latest funding rounds | Hermes native web research |

Notes:
- FRED's API takes its key in the URL (`api_key=`), and the platform
  offers no FRED key, so use FRED's keyless CSV download instead (below).
- Exchange API keys are per-user and should be requested only for authenticated trading actions.
- Do not rely on a shared Massive/Polygon key in the platform baseline.

## Workflow

1. Pick the right live source.
2. Pull the data with a short Python snippet in your virtualenv.
3. If the user wants an image, build a Plotly chart and save it as `.jpg` under `~/charts/`.
4. If event odds or private-company valuation matter, augment with:
   - `polymarket-finite` for prediction-market probabilities
   - Hermes native web research for live funding and valuation research
5. Attach the chart to your reply with `MEDIA:~/charts/<name>.jpg` (an absolute path works too).

## Core Snippets

### yfinance OHLCV

```python
import yfinance as yf

def get_ohlcv(ticker, period="3mo", interval="1d"):
    df = yf.download(ticker, period=period, interval=interval, progress=False)
    df.columns = [c[0].lower() if isinstance(c, tuple) else c.lower() for c in df.columns]
    return df
```

### FRED macro data

```python
import pandas as pd

def fred_series(series_id):
    # FRED's public graph CSV: no key, the series' whole history
    url = f"https://fred.stlouisfed.org/graph/fredgraph.csv?id={series_id}"
    df = pd.read_csv(url, parse_dates=[0], index_col=0)
    return pd.to_numeric(df.iloc[:, 0], errors="coerce").dropna()

series = fred_series("FEDFUNDS").tail(24)
```

Useful series:
- `FEDFUNDS`
- `CPIAUCSL`
- `CPILFESL`
- `UNRATE`
- `GDP`
- `PCE`
- `GS10`
- `GS2`
- `T10Y2Y`
- `VIXCLS`
- `M2SL`
- `DTWEXBGS`

### CCXT OHLCV / orderbook

```python
import ccxt
import pandas as pd

exchange = ccxt.binance({"enableRateLimit": True})
bars = exchange.fetch_ohlcv("BTC/USDT", timeframe="1d", limit=200)
df = pd.DataFrame(bars, columns=["timestamp", "open", "high", "low", "close", "volume"])
df["timestamp"] = pd.to_datetime(df["timestamp"], unit="ms")
df.set_index("timestamp", inplace=True)

book = exchange.fetch_order_book("BTC/USDT")
```

## Charting

Use a dark Plotly default and export charts as JPEG:

```python
import plotly.graph_objects as go
from plotly.subplots import make_subplots
from PIL import Image

CHART_DEFAULTS = dict(
    template="plotly_dark",
    width=1200,
    height=650,
    paper_bgcolor="#1a1a2e",
    plot_bgcolor="#16213e",
    font=dict(color="#eaeaea", size=13),
)

def save_chart(fig, path="~/charts/chart.jpg"):
    import os
    path = os.path.expanduser(path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fig.update_layout(**CHART_DEFAULTS)
    png_path = path.replace(".jpg", ".png")
    fig.write_image(png_path)
    Image.open(png_path).convert("RGB").save(path, "JPEG", quality=95)
    return path
```

Candlestick + volume:

```python
fig = make_subplots(rows=2, cols=1, shared_xaxes=True, row_heights=[0.75, 0.25], vertical_spacing=0.02)
fig.add_trace(go.Candlestick(
    x=df.index,
    open=df["open"],
    high=df["high"],
    low=df["low"],
    close=df["close"],
    increasing_line_color="#26a69a",
    decreasing_line_color="#ef5350",
    name="Price",
), row=1, col=1)
fig.add_trace(go.Bar(
    x=df.index,
    y=df["volume"],
    marker_color=["#26a69a" if c >= o else "#ef5350" for c, o in zip(df["close"], df["open"])],
    name="Volume",
), row=2, col=1)
fig.update_layout(xaxis_rangeslider_visible=False)
```

## Managed Helper Integrations

Each helper lives in its own skill: load that skill (`skill_view`) to get
its directory, and run its script from there.

### Prediction-market overlay

Use `polymarket-finite` (load with `skill_view`) instead of ad hoc Polymarket requests.

Examples, from the polymarket-finite skill's directory:

```bash
python3 scripts/polymarket.py search \
  --query "OpenAI IPO" \
  --limit 5

python3 scripts/polymarket.py market \
  --slug openai-1t-ipo-before-2027
```

### Private-company valuation research

Use Hermes native web tools, prioritize primary filings or company announcements, and link the sources. If the configured backend needs an unavailable operator research credential, state that and use an available public source.

## Delivery Rules

- Save charts you send as `.jpg`, not `.png`.
- Use descriptive filenames such as:
  - `~/charts/btc-daily-chart.jpg`
  - `~/charts/openai-ipo-odds.jpg`
  - `~/charts/macro-dashboard.jpg`
- Include source links after the chart when possible.
- Do not present stale model memory as live market truth.

## Pitfalls

- `yfinance` may return multi-index columns; flatten them immediately.
- `ccxt` should always be initialized with `enableRateLimit=True`.
- FRED's CSV download is rate limited and occasionally slow: fetch each series once per task.
- Thin Polymarket markets are useful as sentiment, not as exact truth.
- Private-company valuation claims should come from live research, not memory.
