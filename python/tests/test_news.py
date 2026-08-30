from app.news import tag_catalysts


def test_offering_dilution_detected_from_a_real_swvl_headline():
    # Actual headline pulled live from Alpaca's news endpoint 2026-08-30
    # while building this — not a fabricated example.
    headline = "Swvl Holdings Announces $1.5M Private Placement Of Shares To Sofico Holdings At $1.46 Per Share"
    assert "offering_dilution" in tag_catalysts(headline)


def test_earnings_keyword_detected():
    assert "earnings" in tag_catalysts("Company Reports Q2 Earnings, Beats EPS Estimates")


def test_fda_keyword_detected():
    assert "fda" in tag_catalysts("Biotech Announces FDA Approval For Phase 3 Trial Results")


def test_merger_keyword_detected():
    assert "merger_acquisition" in tag_catalysts("Company To Be Acquired In All-Cash Merger Deal")


def test_halt_keyword_detected():
    assert "halt_resumption" in tag_catalysts("Trading Halt: News Pending On XYZ")


def test_analyst_action_keyword_detected():
    assert "analyst_action" in tag_catalysts("Analyst Upgrades Stock, Raises Price Target To $10")


def test_irrelevant_headline_tags_nothing():
    # A real headline from the same SWVL news pull, purely a market-movers
    # roundup with no actual catalyst content.
    headline = "12 Industrials Stocks Moving In Friday's Intraday Session"
    assert tag_catalysts(headline) == []


def test_multiple_catalysts_in_one_headline_all_detected():
    text = "Company Reports Earnings Beat And Announces FDA Clearance Same Day"
    tags = tag_catalysts(text)
    assert "earnings" in tags
    assert "fda" in tags


def test_matching_is_case_insensitive():
    assert tag_catalysts("COMPANY ANNOUNCES FDA APPROVAL") == tag_catalysts(
        "company announces fda approval"
    )
