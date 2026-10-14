use market_data::Trade;
use serde_json::json;

fn body() -> serde_json::Value {
    json!({"S":"TEST", "p":1.25, "s":100, "t":"2026-10-14T14:00:00Z", "c":[]})
}

#[test]
fn optional_observational_identity_must_not_drop_an_otherwise_valid_trade() {
    // These formerly ignored metadata values must not change the detector's
    // input stream, including when order flow is disabled.
    for malformed in [json!("unknown-id"), json!(-1), json!(1.5), json!({"unexpected":true}), json!([1,2])] {
        let mut input = body();
        input["i"] = malformed;
        let trade = serde_json::from_value::<Trade>(input)
            .expect("invalid optional identity must not reject the market trade");
        assert_eq!(trade.symbol, "TEST");
        assert_eq!(trade.size, 100);
        assert_eq!(trade.price, 1.25);
        assert_eq!(trade.id, None);
    }
}

#[test]
fn provider_numeric_identity_is_preserved_without_invention_or_precision_loss() {
    let mut input = body();
    input["i"] = json!(u64::MAX);
    assert_eq!(serde_json::from_value::<Trade>(input).unwrap().id, Some(u64::MAX));
    assert_eq!(serde_json::from_value::<Trade>(body()).unwrap().id, None);
    let mut input = body();
    input["i"] = serde_json::Value::Null;
    assert_eq!(serde_json::from_value::<Trade>(input).unwrap().id, None);
}
