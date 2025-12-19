//! Tsume (checkmate puzzle) generator for Wild Cat Shogi.
//!
//! Simulates a game between a high-rated player (Black) and a low-rated player (White).
//! The low-rated player uses MultiPV to select the worst move from the top K moves.
//! The resulting tsume is the SFEN of the position before checkmate.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use shogi::sfen::mirror_sfen;
use shogi::wildcatshogi::{Move, Position, STARTING_SFEN};
use usi::{
    BestMoveParams, EngineCommand, GuiCommand, InfoParams, ScoreKind, ThinkParams,
    UsiEngineHandler,
};

const VARIANTS_INI_PATH: &str = "../../variants.ini";
const FAIRY_STOCKFISH: &str = "fairy-stockfish";
const MAX_MOVES: usize = 300;
const MULTIPV_K: i32 = 5;
const SEARCH_TIME_MS: u64 = 10;
const MAX_ATTEMPTS: usize = 10;

/// Extract just the position SFEN (without move history) from a full SFEN string.
fn position_only_sfen(sfen: &str) -> String {
    if let Some(idx) = sfen.find(" moves") {
        sfen[..idx].to_string()
    } else {
        sfen.to_string()
    }
}

/// Convert wildcatshogi move file numbers between Fairy-Stockfish and library conventions.
///
/// Fairy-Stockfish uses: file 1 = rightmost, file 3 = leftmost
/// shogi-rs uses: file 1 = leftmost, file 3 = rightmost
///
/// Conversion formula: new_file = 4 - old_file
fn convert_move_files(sfen: &str) -> String {
    let chars: Vec<char> = sfen.chars().collect();

    // Drop move (e.g., "P*2b")
    if chars.len() >= 4 && chars[1] == '*' {
        let piece = chars[0];
        let dest_file = chars[2];
        let dest_rank = chars[3];

        if let Some(digit) = dest_file.to_digit(10) {
            let converted_file = 4 - digit;
            return format!("{}*{}{}", piece, converted_file, dest_rank);
        }
        return sfen.to_string();
    }

    // Normal move (e.g., "3a1c" or "3a1c+")
    if chars.len() >= 4 {
        let from_file = chars[0];
        let from_rank = chars[1];
        let to_file = chars[2];
        let to_rank = chars[3];
        let promotion = if chars.len() > 4 { &sfen[4..] } else { "" };

        if let (Some(ff), Some(tf)) = (from_file.to_digit(10), to_file.to_digit(10)) {
            let converted_from = 4 - ff;
            let converted_to = 4 - tf;
            return format!(
                "{}{}{}{}{}",
                converted_from, from_rank, converted_to, to_rank, promotion
            );
        }
    }

    sfen.to_string()
}

/// Collected info from engine during a search.
#[derive(Debug, Clone)]
struct PvInfo {
    multipv: i32,
    score: i32,
    moves: Vec<String>,
}

/// Engine wrapper that maintains communication channels.
struct Engine {
    handler: UsiEngineHandler,
    rx: Receiver<EngineCommand>,
}

/// Result of a search - either a move or game end
#[derive(Debug)]
enum SearchResult {
    Move(String),
    Checkmate,
    Resign,
}

impl Engine {
    fn spawn() -> Option<Self> {
        let mut handler =
            UsiEngineHandler::spawn(FAIRY_STOCKFISH, ".", &["load", VARIANTS_INI_PATH]).ok()?;

        // Set protocol to USI before handshake (required for fairy-stockfish)
        handler
            .send_command_before_handshake(&GuiCommand::SetOption(
                "Protocol".to_string(),
                Some("usi".to_string()),
            ))
            .ok()?;

        // Complete handshake
        handler.get_info().ok()?;

        // Set variant AFTER handshake
        handler
            .send_command(&GuiCommand::SetOption(
                "UCI_Variant".to_string(),
                Some("wildcatshogi".to_string()),
            ))
            .ok()?;

        // Set MultiPV for getting multiple moves
        handler
            .send_command(&GuiCommand::SetOption(
                "MultiPV".to_string(),
                Some(MULTIPV_K.to_string()),
            ))
            .ok()?;

        // Disable contempt for objective play
        handler
            .send_command(&GuiCommand::SetOption(
                "Contempt".to_string(),
                Some("0".to_string()),
            ))
            .ok()?;

        // Penalize draws heavily to encourage decisive games
        handler
            .send_command(&GuiCommand::SetOption(
                "DrawScore".to_string(),
                Some("1000".to_string()),
            ))
            .ok()?;

        // Disable resignation (set to minimum so engine never resigns)
        handler
            .send_command(&GuiCommand::SetOption(
                "ResignValue".to_string(),
                Some("-32767".to_string()),
            ))
            .ok()?;

        // Analysis mode prevents early exit on mate
        handler
            .send_command(&GuiCommand::SetOption(
                "UCI_AnalyseMode".to_string(),
                Some("true".to_string()),
            ))
            .ok()?;

        // TsumeMode disables try rule, focuses on checkmate only
        handler
            .send_command(&GuiCommand::SetOption(
                "TsumeMode".to_string(),
                Some("true".to_string()),
            ))
            .ok()?;

        handler.prepare().ok()?;
        handler.send_command(&GuiCommand::UsiNewGame).ok()?;

        // Set up listener channel
        let (tx, rx): (Sender<EngineCommand>, Receiver<EngineCommand>) = channel();

        handler
            .listen(move |output| -> Result<(), std::io::Error> {
                if let Some(cmd) = output.response() {
                    let _ = tx.send(cmd.clone());
                }
                Ok(())
            })
            .ok()?;

        Some(Engine { handler, rx })
    }

    fn set_position(&mut self, move_history: &[String]) -> Option<()> {
        // Note: GuiCommand::Position already prepends "position sfen"
        let sfen = if move_history.is_empty() {
            STARTING_SFEN.to_string()
        } else {
            format!("{} moves {}", STARTING_SFEN, move_history.join(" "))
        };
        self.handler.send_command(&GuiCommand::Position(sfen)).ok()
    }

    fn search_with_time(&mut self, time_ms: u64) -> Option<(Vec<PvInfo>, SearchResult)> {
        // Start search with time limit
        let params = ThinkParams::new().byoyomi(Duration::from_millis(time_ms));
        self.handler.send_command(&GuiCommand::Go(params)).ok()?;

        // Collect PV info and wait for bestmove
        let mut pv_infos: Vec<PvInfo> = Vec::new();
        let mut current_multipv: i32 = 1;
        let mut current_score: i32 = 0;
        let mut current_moves: Vec<String> = Vec::new();

        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(cmd) => match cmd {
                    EngineCommand::Info(params) => {
                        for param in params {
                            match param {
                                InfoParams::MultiPv(pv) => {
                                    current_multipv = pv;
                                }
                                InfoParams::Score(score, kind) => {
                                    current_score = match kind {
                                        ScoreKind::CpExact
                                        | ScoreKind::CpLowerbound
                                        | ScoreKind::CpUpperbound => score,
                                        ScoreKind::MateExact
                                        | ScoreKind::MateSignOnly
                                        | ScoreKind::MateLowerbound
                                        | ScoreKind::MateUpperbound => {
                                            if score > 0 { 10000 } else { -10000 }
                                        }
                                    };
                                }
                                InfoParams::Pv(moves) => {
                                    current_moves = moves;
                                }
                                _ => {}
                            }
                        }
                        // Store PV info when we have moves
                        if !current_moves.is_empty() {
                            if let Some(existing) =
                                pv_infos.iter_mut().find(|p| p.multipv == current_multipv)
                            {
                                existing.score = current_score;
                                existing.moves = current_moves.clone();
                            } else {
                                pv_infos.push(PvInfo {
                                    multipv: current_multipv,
                                    score: current_score,
                                    moves: current_moves.clone(),
                                });
                            }
                        }
                    }
                    EngineCommand::BestMove(params) => {
                        let result = match params {
                            BestMoveParams::MakeMove(mv, _) => SearchResult::Move(mv),
                            BestMoveParams::Resign => SearchResult::Resign,
                            BestMoveParams::Win => SearchResult::Checkmate,
                        };
                        return Some((pv_infos, result));
                    }
                    _ => {}
                },
                Err(_) => {
                    return None;
                }
            }
        }
    }

    fn search(&mut self) -> Option<(Vec<PvInfo>, SearchResult)> {
        // Try with normal time first
        let (pv_infos, result) = self.search_with_time(SEARCH_TIME_MS)?;

        // If we got resign with no PV, retry with longer time
        if matches!(result, SearchResult::Resign) && pv_infos.is_empty() {
            return self.search_with_time(SEARCH_TIME_MS * 5);
        }

        Some((pv_infos, result))
    }

    fn get_best_move(&mut self) -> Option<SearchResult> {
        let (pv_infos, result) = self.search()?;

        // Always prefer PV info regardless of result
        if let Some(pv1) = pv_infos.iter().find(|pv| pv.multipv == 1) {
            if let Some(mv) = pv1.moves.first() {
                return Some(SearchResult::Move(mv.clone()));
            }
        }

        // Fallback to bestmove if PV empty
        match result {
            SearchResult::Move(best_move) => Some(SearchResult::Move(best_move)),
            SearchResult::Checkmate => Some(SearchResult::Checkmate),
            SearchResult::Resign => None, // No move available
        }
    }

    fn get_worst_move(&mut self) -> Option<SearchResult> {
        let (pv_infos, result) = self.search()?;

        // Always prefer PV info - pick worst scoring move
        let worst_move = pv_infos
            .iter()
            .filter(|pv| !pv.moves.is_empty())
            .min_by_key(|pv| pv.score)
            .and_then(|pv| pv.moves.first().cloned());

        if let Some(mv) = worst_move {
            return Some(SearchResult::Move(mv));
        }

        // Fallback to bestmove if PV empty
        match result {
            SearchResult::Move(best_move) => Some(SearchResult::Move(best_move)),
            SearchResult::Checkmate => Some(SearchResult::Checkmate),
            SearchResult::Resign => None, // No move available
        }
    }

}

fn main() {
    use std::env;
    use std::fs::File;
    use std::io::Write;

    let args: Vec<String> = env::args().collect();
    let output_file = args.get(1).map(|s| s.as_str()).unwrap_or("results.sfen");
    let target_count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);

    let mut engine = Engine::spawn().expect("Failed to spawn engine");
    let mut file = File::create(output_file).expect("Failed to create output file");
    let mut count = 0;

    while count < target_count {
        if let Some(sfen) = generate_tsume(&mut engine) {
            writeln!(file, "{}", sfen).expect("Failed to write to file");
            count += 1;
        }
    }

    eprintln!("Done: {} -> {}", count, output_file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_only_sfen() {
        assert_eq!(
            position_only_sfen("bkr/p1p/3/P1P/RKB b - 1"),
            "bkr/p1p/3/P1P/RKB b - 1"
        );
        assert_eq!(
            position_only_sfen("bkr/p1p/3/P1P/RKB b - 1 moves 1e2d 3a2b"),
            "bkr/p1p/3/P1P/RKB b - 1"
        );
    }

    #[test]
    fn test_ensure_black_to_move_already_black() {
        let sfen = "bkr/p1p/3/P1P/RKB b - 1";
        assert_eq!(ensure_black_to_move(sfen), sfen);
    }

    #[test]
    fn test_ensure_black_to_move_white_to_move() {
        let sfen = "bkr/p1p/3/P1P/RKB w - 1";
        let result = ensure_black_to_move(sfen);
        assert!(result.contains(" b "), "Should be Black to move after mirror");
    }

    #[test]
    fn test_convert_move_files_normal() {
        // Fairy-Stockfish "1e2d" -> shogi-rs "3e2d"
        assert_eq!(convert_move_files("1e2d"), "3e2d");
        assert_eq!(convert_move_files("3a2b"), "1a2b");
        assert_eq!(convert_move_files("2c2c"), "2c2c"); // file 2 stays 2
    }

    #[test]
    fn test_convert_move_files_drop() {
        // Fairy-Stockfish "P*2c" -> shogi-rs "P*2c" (file 2 stays 2)
        assert_eq!(convert_move_files("P*2c"), "P*2c");
        assert_eq!(convert_move_files("P*1a"), "P*3a");
        assert_eq!(convert_move_files("B*3e"), "B*1e");
    }

    #[test]
    fn test_convert_move_files_promotion() {
        assert_eq!(convert_move_files("1a1b+"), "3a3b+");
        assert_eq!(convert_move_files("3d3e+"), "1d1e+");
    }
}

/// Result of a single game simulation
enum GameResult {
    /// Game ended in checkmate
    Checkmate(String),
    /// Game did not end within move limit
    NoResult,
    /// Error during simulation
    Error,
}

fn simulate_game(engine: &mut Engine) -> GameResult {
    let mut position = Position::startpos();
    let mut move_history: Vec<String> = Vec::new();
    let mut is_black_turn = true;
    let mut sfen_before_last_move = String::new(); // SFEN before the last move was made

    for _move_num in 0..MAX_MOVES {
        let current_sfen = position_only_sfen(&position.to_sfen());

        if engine.set_position(&move_history).is_none() {
            return GameResult::Error;
        }

        // Black (sente) plays best, White (gote) plays worst -> Black will checkmate White
        let result = if is_black_turn {
            engine.get_best_move()
        } else {
            engine.get_worst_move()
        };

        let result = match result {
            Some(r) => r,
            None => {
                // No legal moves = loss in shogi (no stalemate)
                // If Black lost (White won), flip the board so Black is the attacker
                let sfen = if is_black_turn {
                    let flipped = mirror_sfen(&sfen_before_last_move);
                    let parts: Vec<&str> = flipped.split_whitespace().collect();
                    if parts.len() >= 4 {
                        format!("{} {} {} 1", parts[0], parts[1], parts[2])
                    } else {
                        flipped
                    }
                } else {
                    sfen_before_last_move
                };
                return GameResult::Checkmate(sfen);
            }
        };

        match result {
            SearchResult::Move(chosen_move) => {
                // Save position BEFORE this move (for tsume: position before checkmate)
                sfen_before_last_move = current_sfen.clone();

                let converted_move = convert_move_files(&chosen_move);
                let mv = match Move::from_sfen(&converted_move) {
                    Some(m) => m,
                    None => return GameResult::Error,
                };
                if position.make_move(mv).is_err() {
                    return GameResult::Error;
                }

                move_history.push(chosen_move);
                is_black_turn = !is_black_turn;
            }
            SearchResult::Checkmate => {
                // If White wins (Black lost), flip the board so Black is the attacker
                let sfen = if !is_black_turn {
                    let flipped = mirror_sfen(&sfen_before_last_move);
                    let parts: Vec<&str> = flipped.split_whitespace().collect();
                    if parts.len() >= 4 {
                        format!("{} {} {} 1", parts[0], parts[1], parts[2])
                    } else {
                        flipped
                    }
                } else {
                    sfen_before_last_move
                };
                return GameResult::Checkmate(sfen);
            }
            SearchResult::Resign => {
                // Should not reach here - get_best_move/get_worst_move return None instead
                unreachable!("Resign should be handled by returning None from move functions");
            }
        }
    }

    GameResult::NoResult
}

fn generate_tsume(engine: &mut Engine) -> Option<String> {
    for _attempt in 1..=MAX_ATTEMPTS {
        match simulate_game(engine) {
            GameResult::Checkmate(sfen) => {
                return Some(sfen);
            }
            GameResult::NoResult | GameResult::Error => {}
        }
    }

    None
}
