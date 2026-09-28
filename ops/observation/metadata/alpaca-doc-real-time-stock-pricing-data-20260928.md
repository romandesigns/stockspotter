---
updatedAt: 2026-05-18T07:19:29.000Z
agentTools:
  siteIndex: https://docs.alpaca.markets/llms.txt
  projectIndex: https://docs.alpaca.markets/us/llms.txt
---

# Real-time Stock Data

This API provides stock market data on a websocket stream. This helps receive the most up to date market information that could help your trading strategy to act upon certain market movements. If you wish to access the latest pricing data, using the stream provides much better accuracy and performance than polling the latest historical endpoints.

You can find the general description of the real-time WebSocket Stream [here](https://docs.alpaca.markets/docs/streaming-market-data). This page focuses on the stock stream.

# URL

The URL for the stock stream is

```
wss://stream.data.alpaca.markets/{version}/{feed}
```

Sandbox URL:

```
wss://stream.data.sandbox.alpaca.markets/{version}/{feed}
```

Substitute `{version}/{feed}` to one of the followings:

* `v2/sip`  is the SIP (Securities Information Processor) feed
* `v2/iex` is the IEX (Investors Exchange) feed
* `v2/delayed_sip` is a 15-minute delayed SIP feed
* `v1beta1/boats` is the BOATS (Blue Ocean ATS) feed
* `v1beta1/overnight` is the derived Alpaca overnight feed

These feeds are described [here](https://docs.alpaca.markets/docs/historical-stock-data-1#feed-parameter).

Any attempt to access a data feed not available for your subscription will result in an error during authentication.

# Channels

You can [subscribe](https://docs.alpaca.markets/docs/streaming-market-data#subscription) to the channels described in this section. For example

```json
{"action":"subscribe","trades":["AAPL"],"quotes":["AMD","CLDR"],"bars":["*"]}
```

## Trades

### Schema

| Attribute | Type            | Notes                                                                                                   |
| :-------- | :-------------- | :------------------------------------------------------------------------------------------------------ |
| `T`       | string          | message type, always “t”                                                                                |
| `S`       | string          | symbol                                                                                                  |
| `i`       | int             | trade ID                                                                                                |
| `x`       | string          | exchange code where the trade occurred                                                                  |
| `p`       | number          | trade price                                                                                             |
| `s`       | int             | trade size                                                                                              |
| `c`       | array`<string>` | trade condition                                                                                         |
| `t`       | string          | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp with nanosecond precision |
| `z`       | string          | tape                                                                                                    |

### Example

```json
{
  "T": "t",
  "i": 96921,
  "S": "AAPL",
  "x": "D",
  "p": 126.55,
  "s": 1,
  "t": "2021-02-22T15:51:44.208Z",
  "c": ["@", "I"],
  "z": "C"
}
```

## Quotes

### Schema

| Attribute | Type            | Notes                                                                                                   |
| :-------- | :-------------- | :------------------------------------------------------------------------------------------------------ |
| `T`       | string          | message type, always “q”                                                                                |
| `S`       | string          | symbol                                                                                                  |
| `ax`      | string          | ask exchange code                                                                                       |
| `ap`      | number          | ask price                                                                                               |
| `as`      | int             | ask size in round lots                                                                                  |
| `bx`      | string          | bid exchange code                                                                                       |
| `bp`      | number          | bid price                                                                                               |
| `bs`      | int             | bid size in round lots                                                                                  |
| `c`       | array`<string>` | quote condition                                                                                         |
| `t`       | string          | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp with nanosecond precision |
| `z`       | string          | tape                                                                                                    |

### Example

```json
{
  "T": "q",
  "S": "AMD",
  "bx": "U",
  "bp": 87.66,
  "bs": 1,
  "ax": "Q",
  "ap": 87.68,
  "as": 4,
  "t": "2021-02-22T15:51:45.335689322Z",
  "c": ["R"],
  "z": "C"
}
```

## Bars

There are three separate channels where you can stream trade aggregates (bars).

#### Minute Bars (`bars`)

Minute bars are emitted right after each minute mark. They contain the trades from the previous minute. Trades from pre-market and aftermarket are also aggregated and sent out on the bars channel.

Note: Understanding which trades are excluded from minute bars is crucial for accurate data analysis. For more detailed information on how minute bars are calculated and excluded trades, please refer to this article [Stock Minute Bars](https://alpaca.markets/learn/stock-minute-bars/).

#### Daily Bars (`dailyBars`)

Daily bars are emitted right after each minute mark after the market opens. The daily bars contain all trades until the time they were emitted.

#### Updated Bars (`updatedBars`)

Updated bars are emitted after each half-minute mark if a “late” trade arrived after the previous minute mark. For example if a trade with a timestamp of `16:49:59.998` arrived right after `16:50:00`, just after `16:50:30` an updated bar with `t` set to `16:49:00` will be sent containing that trade, possibly updating the previous bar’s closing price and volume.

### Schema

| Attribute | Type   | Description                                                                   |
| :-------- | :----- | :---------------------------------------------------------------------------- |
| `T`       | string | message type: “b”, “d” or “u”                                                 |
| `S`       | string | symbol                                                                        |
| `o`       | number | open price                                                                    |
| `h`       | number | high price                                                                    |
| `l`       | number | low price                                                                     |
| `c`       | number | close price                                                                   |
| `v`       | int    | volume                                                                        |
| `vw`      | number | volume-weighted average price                                                 |
| `n`       | int    | number of trades                                                              |
| `t`       | string | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp |

### Example

```json
{
  "T": "b",
  "S": "SPY",
  "o": 388.985,
  "h": 389.13,
  "l": 388.975,
  "c": 389.12,
  "v": 49378,
  "n": 461,
  "vw": 389.062639,
  "t": "2021-02-22T19:15:00Z"
}
```

## Trade Corrections

These messages indicate that a previously sent trade was incorrect and they contain the corrected trade.

Subscription to trade corrections and cancel/errors is automatic when you subscribe to the trade channel.

```
{"action":"subscribe","trades":["AAPL"]}
[{"T":"subscription","trades":["AAPL"],"quotes":[],"bars":[],"updatedBars":[],"dailyBars":[],"statuses":[],"lulds":[],
"corrections":["AAPL"],"cancelErrors":["AAPL"]}]
```

### Schema

| Attribute | Type            | Description                                                                   |
| :-------- | :-------------- | :---------------------------------------------------------------------------- |
| `T`       | string          | message type, always “c”                                                      |
| `S`       | string          | symbol                                                                        |
| `x`       | string          | exchange code                                                                 |
| `oi`      | int             | original trade id                                                             |
| `op`      | number          | original trade price                                                          |
| `os`      | int             | original trade size                                                           |
| `oc`      | array`<string>` | original trade conditions                                                     |
| `ci`      | int             | corrected trade id                                                            |
| `cp`      | number          | corrected trade price                                                         |
| `cs`      | int             | corrected trade size                                                          |
| `cc`      | array`<string>` | corrected trade conditions                                                    |
| `t`       | string          | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp |
| `z`       | string          | tape                                                                          |

### Example

```json
{
  "T": "c",
  "S": "EEM",
  "x": "M",
  "oi": 52983525033527,
  "op": 39.1582,
  "os": 440000,
  "oc": [
    " ",
    "7"
  ],
  "ci": 52983525034326,
  "cp": 39.1809,
  "cs": 440000,
  "cc": [
    " ",
    "7"
  ],
  "z": "B",
  "t": "2023-04-06T14:25:06.542305024Z"
}
```

## Trade Cancels/Errors

These messages indicate that a previously sent trade was canceled.

Subscription to trade corrections and cancel/errors is automatic when you subscribe to the trade channel.

```
{"action":"subscribe","trades":["AAPL"]}
[{"T":"subscription","trades":["AAPL"],"quotes":[],"bars":[],"updatedBars":[],"dailyBars":[],"statuses":[],"lulds":[],
"corrections":["AAPL"],"cancelErrors":["AAPL"]}]
```

### Schema

| Attribute | Type   | Description                                                                   |
| :-------- | :----- | :---------------------------------------------------------------------------- |
| `T`       | string | message type, always “x”                                                      |
| `S`       | string | symbol                                                                        |
| `i`       | int    | trade id                                                                      |
| `x`       | string | trade exchange                                                                |
| `p`       | number | trade price                                                                   |
| `s`       | int    | trade size                                                                    |
| `a`       | string | action (“C” for cancel, “E” for error)                                        |
| `t`       | string | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp |
| `z`       | string | tape                                                                          |

### Example

```json
{
  "T": "x",
  "S": "GOOGL",
  "i": 465,
  "x": "D",
  "p": 105.31,
  "s": 300,
  "a": "C",
  "z": "C",
  "t": "2023-04-06T13:15:42.83540958Z"
}
```

## LULDs

Limit Up - Limit Down messages provide upper and lower limit price bands to securities.

### Schema

| Attribute | Type   | Description                                                                   |
| :-------- | :----- | :---------------------------------------------------------------------------- |
| `T`       | string | message type, always “l”                                                      |
| `S`       | string | symbol                                                                        |
| `u`       | number | limit up price                                                                |
| `d`       | number | limit down price                                                              |
| `i`       | string | indicator                                                                     |
| `t`       | string | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp |
| `z`       | string | tape                                                                          |

### Example

```json
{
  "T": "l",
  "S": "IONM",
  "u": 3.24,
  "d": 2.65,
  "i": "B",
  "t": "2023-04-06T13:34:45.565004401Z",
  "z": "C"
}
```

## Trading Status

Identifies the trading status applicable to the security and reason for the trading halt if any. The status messages can be accessed from any \{source} depending on your subscription.

To enable market data on a production environment please reach out to our sales team.

### Schema

| Attribute | Type   | Description                                                                   |
| :-------- | :----- | :---------------------------------------------------------------------------- |
| `T`       | string | message type, always “s”                                                      |
| `S`       | string | symbol                                                                        |
| `sc`      | string | status code                                                                   |
| `sm`      | string | status message                                                                |
| `rc`      | string | reason code                                                                   |
| `rm`      | string | reason message                                                                |
| `t`       | string | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp |
| `z`       | string | tape                                                                          |

### Example

```json
{
  "T": "s",
  "S": "AAPL",
  "sc": "H",
  "sm": "Trading Halt",
  "rc": "T12",
  "rm": "Trading Halted; For information requested by NASDAQ",
  "t": "2021-02-22T19:15:00Z",
  "z": "C"
}
```

### Status Codes

#### Tape A & B (CTA)

| Code | Value                          |
| :--- | :----------------------------- |
| 2    | Trading Halt                   |
| 3    | Resume                         |
| 5    | Price Indication               |
| 6    | Trading Range Indication       |
| 7    | Market Imbalance Buy           |
| 8    | Market Imbalance Sell          |
| 9    | Market On Close Imbalance Buy  |
| A    | Market On Close Imbalance Sell |
| C    | No Market Imbalance            |
| D    | No Market On Close Imbalance   |
| E    | Short Sale Restriction         |
| F    | Limit Up-Limit Down            |

#### Tape C & O (UTP)

| Codes | Resume                   |
| :---- | :----------------------- |
| H     | Trading Halt             |
| Q     | Quotation Resumption     |
| T     | Trading Resumption       |
| P     | Volatility Trading Pause |

### Reason Codes

#### Tape A & B (CTA)

| Code | Value                                          |
| :--- | :--------------------------------------------- |
| A    | Additional Information Requested               |
| C    | Regulatory Concern                             |
| D    | News Released (formerly News Dissemination)    |
| E    | Merger Effective                               |
| F    | ETF Component Prices Not Available             |
| I    | Order Imbalance                                |
| M    | Limit Up-Limit Down (LULD) Trading Pause       |
| N    | Corporate Action                               |
| O    | New Security Offering                          |
| P    | News Pending                                   |
| V    | Intraday Indicative Value Not Available        |
| X    | Operational                                    |
| Y    | Sub-Penny Trading                              |
| 1    | Market-Wide Circuit Breaker Level 1 – Breached |
| 2    | Market-Wide Circuit Breaker Level 2 – Breached |
| 3    | Market-Wide Circuit Breaker Level 3 – Breached |

#### Tape C & O (UTP)

| Code | Value                                                                 |
| :--- | :-------------------------------------------------------------------- |
| T1   | Halt News Pending                                                     |
| T2   | Halt News Dissemination                                               |
| T5   | Single Stock Trading Pause In Affect                                  |
| T6   | Regulatory Halt Extraordinary Market Activity                         |
| T8   | Halt ETF                                                              |
| T12  | Trading Halted; For information requested by NASDAQ                   |
| H4   | Halt Non Compliance                                                   |
| H9   | Halt Filings Not Current                                              |
| H10  | Halt SEC Trading Suspension                                           |
| H11  | Halt Regulatory Concern                                               |
| 01   | Operations Halt, Contact Market Operations                            |
| IPO1 | IPO Issue not yet Trading                                             |
| M1   | Corporate Action                                                      |
| M2   | Quotation Not Available                                               |
| LUDP | Volatility Trading Pause                                              |
| LUDS | Volatility Trading Pause – Straddle Condition                         |
| MWC1 | Market Wide Circuit Breaker Halt – Level 1                            |
| MWC2 | Market Wide Circuit Breaker Halt – Level 2                            |
| MWC3 | Market Wide Circuit Breaker Halt – Level 3                            |
| MWC0 | Market Wide Circuit Breaker Halt – Carry over from previous day       |
| T3   | News and Resumption Times                                             |
| T7   | Single Stock Trading Pause/Quotation-Only Period                      |
| R4   | Qualifications Issues Reviewed/Resolved; Quotations/Trading to Resume |
| R9   | Filing Requirements Satisfied/Resolved; Quotations/Trading To Resume  |
| C3   | Issuer News Not Forthcoming; Quotations/Trading To Resume             |
| C4   | Qualifications Halt ended; maint. Req. met; Resume                    |
| C9   | Qualifications Halt Concluded; Filings Met; Quotes/Trades To Resume   |
| C11  | Trade Halt Concluded By Other Regulatory Auth,; Quotes/Trades Resume  |
| R1   | New Issue Available                                                   |
| R    | Issue Available                                                       |
| IPOQ | IPO security released for quotation                                   |
| IPOE | IPO security – positioning window extension                           |
| MWCQ | Market Wide Circuit Breaker Resumption                                |

## Order imbalances

Order imbalance is a situation resulting from an excess of buy or sell orders for a specific security on a trading exchange, making it impossible to match the orders of buyers and sellers. Order imbalance messages are typically sent during limit-up and limit-down trading halts. You have to subscribe to these messages using the `imbalances` JSON key:

```json
{"action":"subscribe","imbalances":["INAQU"]}
```

### Schema

| Attribute | Type   | Notes                                                                                                   |
| :-------- | :----- | :------------------------------------------------------------------------------------------------------ |
| `T`       | string | message type, always “i”                                                                                |
| `S`       | string | symbol                                                                                                  |
| `p`       | number | price                                                                                                   |
| `z`       | string | tape                                                                                                    |
| `t`       | string | [RFC-3339](https://datatracker.ietf.org/doc/html/rfc3339) formatted timestamp with nanosecond precision |

### Example

```json
{
  "T": "i",
  "S": "INAQU",
  "p": 9.12,
  "z": "C",
  "t": "2024-12-13T19:58:09.242138635Z"
}
```

# Example

```json Shell
$ wscat -c wss://stream.data.alpaca.markets/v2/sip
connected (press CTRL+C to quit)
< [{"T":"success","msg":"connected"}]
> {"action": "auth", "key": "*****", "secret": "*****"}
< [{"T":"success","msg":"authenticated"}]
> {"action": "subscribe", "trades": ["AAPL"], "quotes": ["AMD", "CLDR"], "bars": ["*"],"dailyBars":["VOO"],"statuses":["*"]}
< [{"T":"subscription","trades":["AAPL"],"quotes":["AMD","CLDR"],"bars":["*"],"updatedBars":[],"dailyBars":["VOO"],"statuses":["*"],"lulds":[],"corrections":["AAPL"],"cancelErrors":["AAPL"]}]
< [{"T":"q","S":"AMD","bx":"K","bp":91.95,"bs":2,"ax":"Q","ap":91.98,"as":1,"c":["R"],"z":"C","t":"2023-04-06T11:54:21.670905508Z"}]
< [{"T":"t","S":"AAPL","i":628,"x":"K","p":162.92,"s":3,"c":["@","F","T","I"],"z":"C","t":"2023-04-06T11:54:26.838232225Z"},{"T":"t","S":"AAPL","i":75,"x":"Z","p":162.92,"s":3,"c":["@","F","T","I"],"z":"C","t":"2023-04-06T11:54:26.838562809Z"},{"T":"t","S":"AAPL","i":1465,"x":"P","p":162.91,"s":71,"c":["@","F","T","I"],"z":"C","t":"2023-04-06T11:54:26.83915973Z"}]
< [{"T":"q","S":"AMD","bx":"P","bp":91.9,"bs":1,"ax":"Q","ap":91.98,"as":1,"c":["R"],"z":"C","t":"2023-04-06T11:54:27.924933876Z"}]
```