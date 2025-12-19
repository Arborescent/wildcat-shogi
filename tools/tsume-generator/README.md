# Tsume Generator

Generates mate-in-1 tsume (checkmate puzzles) for Wild Cat Shogi. Designed for casual play.

## How It Works

1. Spawns Fairy-Stockfish with Wild Cat Shogi variant
2. Simulates games where:
   - Black (sente) plays the best moves
   - White (gote) plays the worst moves from MultiPV
3. Continues until checkmate occurs
4. Returns the position before the final checkmate move (Black to play)
5. If White wins instead, flips the board so Black is always the attacker

## Build

```bash
cargo build --release
```

## Usage

### Single instance

```bash
./target/release/tsume-generator <output_file> [count]
```

- `output_file`: Path to output SFEN file (default: `results.sfen`)
- `count`: Number of puzzles to generate (default: 1000)

### Parallel generation

```bash
./generate.sh <output_file> [count]
```

Spawns 16 workers in parallel, divides the work, deduplicates, and concatenates results.

```bash
# Generate 1000 puzzles to results.sfen
./generate.sh results.sfen 1000

# Generate 10000 puzzles to puzzles.sfen
./generate.sh puzzles.sfen 10000
```

Note: Each worker reuses a single Fairy-Stockfish process for all its puzzles. The final output is deduplicated, so the actual count may be slightly less than requested.

## Output Format

One SFEN per line, Black to play, representing a position where Black can force checkmate.

```
rkb/1p1/3/P1P/BKR b - 1
k2/PBR/3/p1p/rbK b - 1
...
```

## Configuration

Constants in `src/main.rs`:

| Constant | Default | Description |
|----------|---------|-------------|
| `MAX_MOVES` | 300 | Maximum moves per game before giving up |
| `MULTIPV_K` | 5 | Number of moves to consider for worst-move selection |
| `SEARCH_TIME_MS` | 10 | Milliseconds per move search |
| `MAX_ATTEMPTS` | 10 | Retry attempts per puzzle |

## Engine Options

The generator sets these Fairy-Stockfish options:

- `UCI_Variant`: wildcatshogi
- `MultiPV`: 5 (for worst-move selection)
- `Contempt`: 0 (objective play)
- `DrawScore`: 1000 (penalize draws)
- `ResignValue`: -32767 (never resign)
- `UCI_AnalyseMode`: true (prevent early exit)
- `TsumeMode`: true (checkmate-only wins, no try rule)

## Requirements

- [Fairy-Stockfish](https://github.com/fairy-stockfish/Fairy-Stockfish) in PATH
- `variants.ini` with Wild Cat Shogi definition (expected at `../../variants.ini`)
