# stockspotter mobile

1. Copy `.env.example` to `.env` and replace the sample IP with the machine running `ws-server`.
2. From the repository root, run `bun run dev:mobile`.
3. Open the QR code in Expo Go, or press `a`/`i` for a local simulator.

The phone and backend must be able to reach each other. `localhost` on a physical phone points to the phone itself, not the development machine.
