// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// ============================================================================
/// ⚠️  UNAUDITED — deploy to Base Sepolia first, tiny wagers, audit before mainnet.
/// ============================================================================
///
/// MatchEscrow (spec §12): holds two equal antes for a 1v1 match and releases
/// the pot according to a result signed by the authorized game server. The
/// chain never sees gameplay — only money (spec §11).
///
/// Antes can be NATIVE ETH (token == address(0)) or any allow-listed ERC-20
/// (USDC on Base being the intended one). Per-token max wager doubles as the
/// allow-list: maxWager[token] == 0 means the token isn't accepted.
///
/// Principles: pull-based payouts, reentrancy guard, no external calls before
/// state changes, emergency pause, authorized result signer, refund timeout,
/// low beta caps.
interface IERC20 {
    function transferFrom(address from, address to, uint256 value) external returns (bool);
    function transfer(address to, uint256 value) external returns (bool);
}

contract MatchEscrow {
    address public constant NATIVE = address(0); // ETH ante marker

    enum Status { None, Created, Locked, Settled, Cancelled, Voided }

    struct Match {
        address p1;
        address p2;
        address token;      // NATIVE for ETH, else ERC-20 (e.g. USDC)
        uint96 wager;       // per-player ante in the token's smallest unit
        Status status;
        address winner;
        uint64 createdAt;
        uint16 feeBpsSnap;  // fee locked in at createMatch — later setFee never touches in-flight matches
    }

    address public owner;
    address public pendingOwner;             // 2-step ownership transfer
    address public resultSigner;             // the game server's settlement key
    uint16 public feeBps;                    // protocol fee on the pot (future matches)
    address public feeRecipient;
    uint64 public refundTimeout = 1 hours;   // no result by then -> self-refund
    bool public paused;

    mapping(address => uint96) public maxWager;                    // token -> cap (0 = not allowed)
    mapping(bytes32 => Match) public matches;                      // matchId -> match
    mapping(address => mapping(address => uint256)) public claimable; // user -> token -> amount

    event MatchCreated(bytes32 indexed matchId, address indexed p1, address token, uint96 wager);
    event MatchJoined(bytes32 indexed matchId, address indexed p2);
    event MatchSettled(bytes32 indexed matchId, address indexed winner, uint256 pot, uint256 fee);
    event MatchCancelled(bytes32 indexed matchId);
    event MatchVoided(bytes32 indexed matchId);
    event Claimed(address indexed who, address token, uint256 amount);
    event FeeUpdated(uint16 feeBps, address feeRecipient);
    event OwnershipTransferStarted(address indexed from, address indexed to);
    event OwnershipTransferred(address indexed from, address indexed to);

    uint256 private _lock = 1;
    modifier nonReentrant() {
        require(_lock == 1, "reentrant");
        _lock = 2;
        _;
        _lock = 1;
    }
    modifier onlyOwner() { require(msg.sender == owner, "not owner"); _; }
    modifier notPaused() { require(!paused, "paused"); _; }

    constructor(address _resultSigner, address _feeRecipient, uint16 _feeBps) {
        require(_feeBps <= 1000, "fee too high"); // <= 10%
        require(_feeRecipient != address(0), "zero recipient");
        owner = msg.sender;
        resultSigner = _resultSigner;
        feeRecipient = _feeRecipient;
        feeBps = _feeBps;
    }

    /// Player 1 opens a match and antes in. matchId comes from the game server
    /// so server and chain reference the same duel.
    function createMatch(bytes32 matchId, address token, uint96 wager)
        external
        payable
        notPaused
        nonReentrant
    {
        require(matches[matchId].status == Status.None, "exists");
        require(wager > 0 && wager <= maxWager[token], "bad wager/token");
        matches[matchId] = Match(
            msg.sender, address(0), token, wager, Status.Created, address(0), uint64(block.timestamp), feeBps
        );
        _collect(token, wager);
        emit MatchCreated(matchId, msg.sender, token, wager);
    }

    /// Player 2 matches the ante; escrow locks and the duel may start.
    function joinMatch(bytes32 matchId) external payable notPaused nonReentrant {
        Match storage m = matches[matchId];
        require(m.status == Status.Created, "not joinable");
        require(msg.sender != m.p1, "self");
        m.p2 = msg.sender;
        m.status = Status.Locked;
        _collect(m.token, m.wager);
        emit MatchJoined(matchId, msg.sender);
    }

    /// Creator may cancel while unjoined; ante returns to claimable.
    function cancelMatch(bytes32 matchId) external nonReentrant {
        Match storage m = matches[matchId];
        require(m.status == Status.Created && msg.sender == m.p1, "no");
        m.status = Status.Cancelled;
        claimable[m.p1][m.token] += m.wager;
        emit MatchCancelled(matchId);
    }

    /// Anyone may relay the signed outcome (typically the winner's browser) —
    /// the SIGNATURE is the authority, not the sender.
    /// signed message = eth_personal_sign over keccak256(matchId, winner).
    function submitResult(bytes32 matchId, address winner, bytes calldata sig)
        external
        notPaused
        nonReentrant
    {
        Match storage m = matches[matchId];
        require(m.status == Status.Locked, "not locked");
        require(winner == m.p1 || winner == m.p2, "bad winner");

        bytes32 digest = keccak256(
            abi.encodePacked("\x19Ethereum Signed Message:\n32", keccak256(abi.encodePacked(matchId, winner)))
        );
        require(_recover(digest, sig) == resultSigner, "bad sig");

        m.status = Status.Settled;
        m.winner = winner;
        uint256 pot = uint256(m.wager) * 2;
        uint256 fee = (pot * m.feeBpsSnap) / 10_000; // snapshot: fee the players locked in at
        claimable[winner][m.token] += pot - fee;
        if (fee > 0) claimable[feeRecipient][m.token] += fee;
        emit MatchSettled(matchId, winner, pot - fee, fee);
    }

    /// Same authority as submitResult (a server-signed outcome), but instead of
    /// crediting the winner's pull balance it PUSHES the payout to them in this
    /// same transaction. The game server relays this from a gas-funded wallet,
    /// so the winner receives the pot automatically — no claim tx, no gas, no
    /// wallet popup. The fee still accrues to feeRecipient's pull balance.
    /// If the push fails (e.g. winner contract rejects the transfer) the amount
    /// falls back to the winner's claimable balance so funds are never stuck.
    function settle(bytes32 matchId, address winner, bytes calldata sig)
        external
        notPaused
        nonReentrant
    {
        Match storage m = matches[matchId];
        require(m.status == Status.Locked, "not locked");
        require(winner == m.p1 || winner == m.p2, "bad winner");

        bytes32 digest = keccak256(
            abi.encodePacked("\x19Ethereum Signed Message:\n32", keccak256(abi.encodePacked(matchId, winner)))
        );
        require(_recover(digest, sig) == resultSigner, "bad sig");

        m.status = Status.Settled;
        m.winner = winner;
        uint256 pot = uint256(m.wager) * 2;
        uint256 fee = (pot * m.feeBpsSnap) / 10_000; // snapshot, as in submitResult
        if (fee > 0) claimable[feeRecipient][m.token] += fee; // pull, unchanged
        emit MatchSettled(matchId, winner, pot - fee, fee);
        _payout(m.token, winner, pot - fee); // push (interaction last; guarded)
    }

    /// If no result arrives in time (server failure -> refund, spec §10),
    /// either player can void the match and both antes become claimable.
    function refund(bytes32 matchId) external nonReentrant {
        Match storage m = matches[matchId];
        require(m.status == Status.Locked, "not locked");
        require(msg.sender == m.p1 || msg.sender == m.p2, "not a player");
        require(block.timestamp >= m.createdAt + refundTimeout, "too early");
        m.status = Status.Voided;
        claimable[m.p1][m.token] += m.wager;
        claimable[m.p2][m.token] += m.wager;
        emit MatchVoided(matchId);
    }

    /// Pull-based payout, per token.
    function claimPayout(address token) external nonReentrant {
        uint256 amount = claimable[msg.sender][token];
        require(amount > 0, "nothing");
        claimable[msg.sender][token] = 0;
        if (token == NATIVE) {
            (bool ok, ) = msg.sender.call{ value: amount }("");
            require(ok, "eth send failed");
        } else {
            require(IERC20(token).transfer(msg.sender, amount), "transfer failed");
        }
        emit Claimed(msg.sender, token, amount);
    }

    // ── admin ──
    function setPaused(bool v) external onlyOwner { paused = v; }
    function setResultSigner(address s) external onlyOwner { resultSigner = s; }
    /// Allow-list a token (NATIVE for ETH) with its beta wager cap; 0 disables.
    function setMaxWager(address token, uint96 cap) external onlyOwner { maxWager[token] = cap; }

    /// Adjust the protocol fee for FUTURE matches only — in-flight matches keep
    /// the feeBps snapshotted at createMatch.
    function setFee(uint16 bps, address recipient) external onlyOwner {
        require(bps <= 1000, "fee too high"); // <= 10%
        require(recipient != address(0), "zero recipient");
        feeBps = bps;
        feeRecipient = recipient;
        emit FeeUpdated(bps, recipient);
    }

    /// 2-step ownership transfer so the fee stream can't be bricked by a
    /// fat-fingered owner change.
    function transferOwnership(address to) external onlyOwner {
        pendingOwner = to;
        emit OwnershipTransferStarted(owner, to);
    }

    function acceptOwnership() external {
        require(msg.sender == pendingOwner, "not pending owner");
        emit OwnershipTransferred(owner, msg.sender);
        owner = msg.sender;
        pendingOwner = address(0);
    }

    /// Push `amount` of `token` to `to`. On any failure, credit `to`'s pull
    /// balance instead so funds are never trapped (the winner can claimPayout).
    function _payout(address token, address to, uint256 amount) private {
        if (amount == 0) return;
        bool ok;
        if (token == NATIVE) {
            (ok, ) = to.call{ value: amount }("");
        } else {
            (bool called, bytes memory ret) = token.call(
                abi.encodeWithSelector(IERC20.transfer.selector, to, amount)
            );
            ok = called && (ret.length == 0 || abi.decode(ret, (bool)));
        }
        if (!ok) claimable[to][token] += amount;
    }

    function _collect(address token, uint96 wager) private {
        if (token == NATIVE) {
            require(msg.value == wager, "wrong ETH amount");
        } else {
            require(msg.value == 0, "no ETH with token ante");
            require(IERC20(token).transferFrom(msg.sender, address(this), wager), "ante failed");
        }
    }

    function _recover(bytes32 digest, bytes calldata sig) private pure returns (address) {
        require(sig.length == 65, "sig len");
        bytes32 r = bytes32(sig[0:32]);
        bytes32 s = bytes32(sig[32:64]);
        uint8 v = uint8(sig[64]);
        if (v < 27) v += 27;
        require(uint256(s) <= 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0, "bad s");
        return ecrecover(digest, v, r, s);
    }
}
