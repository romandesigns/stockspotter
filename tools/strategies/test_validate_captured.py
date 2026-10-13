import unittest
from validate_captured import outcome

class OutcomeContract(unittest.TestCase):
    def bars(self):
        return [dict(marketOpenMs=1000+i*60000, high=103,low=99) for i in range(5)]

    def test_uses_future_bars_and_stage_specific_price(self):
        event=dict(eventTimeMs=1000,observedPrice=100,anchorPrice=90)
        self.assertEqual(outcome(self.bars(),event)["classification"],"reach")
        event["observedPrice"]=102
        self.assertEqual(outcome(self.bars(),event)["classification"],"no_reach")

    def test_early_reach_does_not_clear_incomplete_or_gapped_horizon(self):
        event=dict(eventTimeMs=1000,observedPrice=100)
        self.assertEqual(outcome(self.bars()[:-1],event)["classification"],"censored")
        self.assertEqual(outcome(self.bars()[1:],event)["classification"],"censored")

    def test_anchor_bar_is_excluded(self):
        event=dict(eventTimeMs=1000,observedPrice=100)
        bars=self.bars()
        for b in bars:b["high"]=101
        bars.insert(0,dict(marketOpenMs=-59000,high=1000,low=99))
        self.assertEqual(outcome(bars,event)["classification"],"no_reach")

if __name__ == "__main__": unittest.main()
