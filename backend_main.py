"""Backend entry point for the Tauri app: `backend_main.py --port N`."""
import argparse
import logging

from app.server import main

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Reimbursement Manager backend")
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)-7s %(message)s",
        datefmt="%H:%M:%S",
    )
    main(args.port)
