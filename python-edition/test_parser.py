from app.parser import parse_receipt

cfg = {"default_currency": "CNY", "local_ocr": {"min_avg_confidence": 0.75}}

chinese = "老成都火锅店\n开票日期：2026-10-08\n¥78.00\n牛肉卷×2\n¥28.00\n蔬菜拼盘\n¥24.00\n饮料x3\n¥130.00\n合计：\n¥14.95\n税频：\n谢谢惠顾"
r = parse_receipt(chinese, 0.97, cfg)
print("CN:", r.date, r.merchant, r.amount, r.tax, r.currency, "review=", r.needs_review)
assert (r.date, r.merchant, r.amount, r.tax) == ("2026-10-08", "老成都火锅店", 130.00, 14.95), "CN FAIL"

english = "STARBUCKS COFFEE\nDate: 10/05/2026\nLatte x2\n$9.50\n$4.25\nCroissant\n$3.75\nBanana Bread\nSubtotal\n$17.50\nax\n$1.58\nTOTAL\n$19.08\nThank You"
r = parse_receipt(english, 0.97, cfg)
print("EN:", r.date, r.merchant, r.amount, r.tax, r.currency, "review=", r.needs_review)
assert r.amount == 19.08 and r.currency == "USD" and r.needs_review, "EN FAIL"

same = "Walmart\n2026/09/15\nGroceries\nTOTAL $42.10\nTax $3.37"
r = parse_receipt(same, 0.99, cfg)
print("SAME:", r.date, r.amount, r.tax, "review=", r.needs_review)
assert (r.date, r.amount, r.tax, r.needs_review) == ("2026-09-15", 42.10, 3.37, False), "SAME FAIL"

vat = "XX科技有限公司\n开票日期：2026年09月20日\n税率 13%\n价税合计 ¥565.00"
r = parse_receipt(vat, 0.95, cfg)
print("VAT:", r.date, r.amount, r.tax, "review=", r.needs_review)
assert r.amount == 565.0 and r.date == "2026-09-20" and r.tax == 65.0, "VAT FAIL"

plain = "Bookstore\nInvoice No. 12345\n2026-08-01\nBooks\nTotal 25.50"
r = parse_receipt(plain, 0.99, cfg)
print("PLAIN:", r.date, r.amount, "review=", r.needs_review)
assert r.amount == 25.5, "PLAIN FAIL"

# Regression: scrambled totals block from a real RapidOCR scan (Yangtze hotel
# receipt). Labels/values split across lines, 4-digit amounts without
# thousands separators, and 'Sub-total' must not beat 'Total RMB'.
yangtze = """The Yangtze Boutique ShangHai
1933中餐厅
Date : 2026-06-02
1.00 苹果木烟熏鯧鱼/例 128.00
1.30 海钓大黄鱼/斤 634.40
食品
FOOD
酒水
BEV
2250.40
小计
Sub-total
208.00
2458.40
总计
Total RMB
2458.40
房号/账号"""
r = parse_receipt(yangtze, 0.99, cfg)
print("YANGTZE:", r.date, r.amount, r.currency, "review=", r.needs_review)
assert r.amount == 2458.40, "YANGTZE FAIL (expected 2458.40)"

print("ALL PARSER TESTS PASSED")
