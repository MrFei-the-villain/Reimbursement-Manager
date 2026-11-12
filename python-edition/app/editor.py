"""Interactive editor for existing receipt rows in the Excel file."""
from .excel import ExcelStore
from .models import ReceiptData


def _print_rows(rows):
    print(f"\n{'#':>3}  {'Date':<10}  {'Cur':<4}  {'Amount':>10}  {'Place':<24}")
    print("-" * 62)
    for row_num, r in rows:
        num = row_num - 1
        place = (r.merchant or "?")[:24]
        amount = f"{r.amount:.2f}" if r.amount is not None else "?"
        print(f"{num:>3}  {r.date or '?':<10}  {r.currency or '':<4}  "
              f"{amount:>10}  {place:<24}")
    print("-" * 62)


def _prompt(label, current):
    value = input(f"  {label} [{current or ''}]: ").strip()
    return value if value else current


def _edit_one(store: ExcelStore, row_num: int, r: ReceiptData):
    print("Press Enter to keep the current value.\n")
    r.date = _prompt("Date (YYYY-MM-DD)", r.date)
    r.currency = _prompt("Currency", r.currency)

    amount = _prompt("Amount", r.amount)
    try:
        r.amount = float(amount)
    except (TypeError, ValueError):
        pass  # keep current

    r.merchant = _prompt("Place", r.merchant)

    r.needs_review = False  # manually reviewed
    store.update_row(row_num, r)
    print(f"Row {row_num - 1} updated.\n")


def run_editor(store: ExcelStore):
    rows = store.list_rows()
    if not rows:
        print("No receipts recorded yet — drop some images into the inbox first.")
        return
    _print_rows(rows)
    while True:
        choice = input("\nRow number to edit (blank to quit): ").strip()
        if not choice:
            break
        if not choice.isdigit():
            print("Please enter a number.")
            continue
        target = int(choice) + 1  # data # is excel row - 1 (header is row 1)
        match = next(((n, r) for n, r in rows if n == target), None)
        if match is None:
            print(f"No row #{choice}.")
            continue
        _edit_one(store, match[0], match[1])
        rows = store.list_rows()
        _print_rows(rows)
