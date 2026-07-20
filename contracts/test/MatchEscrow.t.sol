// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {MatchEscrow} from "../src/MatchEscrow.sol";

// ─── helpers ────────────────────────────────────────────────────────────────

/// Minimal 6-decimal ERC-20 standing in for USDC.
contract MockUSDC {
    string public constant symbol = "USDC";
    uint8 public constant decimals = 6;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amt) external {
        balanceOf[to] += amt;
    }

    function approve(address spender, uint256 amt) external returns (bool) {
        allowance[msg.sender][spender] = amt;
        return true;
    }

    function transfer(address to, uint256 amt) external returns (bool) {
        balanceOf[msg.sender] -= amt;
        balanceOf[to] += amt;
        return true;
    }

    function transferFrom(address from, address to, uint256 amt) external returns (bool) {
        allowance[from][msg.sender] -= amt;
        balanceOf[from] -= amt;
        balanceOf[to] += amt;
        return true;
    }
}

/// Attacker that re-enters claimPayout from its ETH receive hook.
contract Reenterer {
    MatchEscrow private immutable esc;

    constructor(MatchEscrow e) {
        esc = e;
    }

    function create(bytes32 matchId) external payable {
        esc.createMatch{value: msg.value}(matchId, address(0), uint96(msg.value));
    }

    function claim() external {
        esc.claimPayout(address(0));
    }

    receive() external payable {
        // second entry must hit the reentrancy guard
        esc.claimPayout(address(0));
    }
}

/// A "winner" contract that rejects direct ETH — settle()'s push must fall back
/// to crediting its claimable balance rather than reverting the whole settle.
contract RejectEther {
    receive() external payable {
        revert("no ether");
    }
}

/// A "winner" contract that tries to re-enter claimPayout from its receive hook
/// while settle() holds the reentrancy lock — the push fails and falls back to
/// claimable (no double-spend).
contract SettleReenterer {
    MatchEscrow private immutable esc;

    constructor(MatchEscrow e) {
        esc = e;
    }

    receive() external payable {
        esc.claimPayout(address(0));
    }
}

// ─── unit tests ─────────────────────────────────────────────────────────────

contract MatchEscrowTest is Test {
    uint256 constant SIGNER_KEY = 0xA11CE;
    uint256 constant ROGUE_KEY = 0xBADD1E;
    uint256 constant SECP256K1_N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    address signer;
    address feeRecipient = makeAddr("feeRecipient");
    address alice = makeAddr("alice");
    address bob = makeAddr("bob");
    address carol = makeAddr("carol");

    MatchEscrow esc;
    MockUSDC usdc;

    uint96 constant CAP_ETH = 1 ether;
    uint96 constant CAP_USDC = 100e6;
    uint96 constant WAGER = 0.05 ether;

    function setUp() public {
        signer = vm.addr(SIGNER_KEY);
        esc = new MatchEscrow(signer, feeRecipient, 250);
        esc.setMaxWager(address(0), CAP_ETH);
        usdc = new MockUSDC();
        esc.setMaxWager(address(usdc), CAP_USDC);
        vm.deal(alice, 10 ether);
        vm.deal(bob, 10 ether);
        vm.deal(carol, 10 ether);
    }

    function _sign(uint256 pk, bytes32 matchId, address winner) internal pure returns (bytes memory) {
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19Ethereum Signed Message:\n32", keccak256(abi.encodePacked(matchId, winner))
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, digest);
        return abi.encodePacked(r, s, v);
    }

    function _createJoin(bytes32 id, uint96 wager) internal {
        vm.prank(alice);
        esc.createMatch{value: wager}(id, address(0), wager);
        vm.prank(bob);
        esc.joinMatch{value: wager}(id);
    }

    function _status(bytes32 id) internal view returns (MatchEscrow.Status s) {
        (,,,, s,,,) = esc.matches(id);
    }

    // ── lifecycle ──

    function test_CreateJoinSettleClaim_ETH() public {
        bytes32 id = keccak256("m1");
        _createJoin(id, WAGER);
        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Locked));

        esc.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));
        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Settled));

        uint256 pot = uint256(WAGER) * 2;
        uint256 fee = (pot * 250) / 10_000;
        assertEq(esc.claimable(alice, address(0)), pot - fee);
        assertEq(esc.claimable(feeRecipient, address(0)), fee);

        uint256 before = alice.balance;
        vm.prank(alice);
        esc.claimPayout(address(0));
        assertEq(alice.balance, before + pot - fee);
        assertEq(esc.claimable(alice, address(0)), 0);
    }

    function test_CancelBeforeJoin() public {
        bytes32 id = keccak256("m2");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(alice);
        esc.cancelMatch(id);
        assertEq(esc.claimable(alice, address(0)), WAGER);
        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Cancelled));
    }

    function test_RevertWhen_CancelAfterLock() public {
        bytes32 id = keccak256("m3");
        _createJoin(id, WAGER);
        vm.prank(alice);
        vm.expectRevert(bytes("no"));
        esc.cancelMatch(id);
    }

    function test_RevertWhen_CancelByNonCreator() public {
        bytes32 id = keccak256("m4");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(bob);
        vm.expectRevert(bytes("no"));
        esc.cancelMatch(id);
    }

    function test_RevertWhen_DoubleCreate() public {
        bytes32 id = keccak256("m5");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(bob);
        vm.expectRevert(bytes("exists"));
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
    }

    function test_RevertWhen_SelfJoin() public {
        bytes32 id = keccak256("m6");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(alice);
        vm.expectRevert(bytes("self"));
        esc.joinMatch{value: WAGER}(id);
    }

    function test_RevertWhen_JoinAfterLock() public {
        bytes32 id = keccak256("m7");
        _createJoin(id, WAGER);
        vm.prank(carol);
        vm.expectRevert(bytes("not joinable"));
        esc.joinMatch{value: WAGER}(id);
    }

    function test_RevertWhen_WagerOverCap() public {
        bytes32 id = keccak256("m8");
        vm.prank(alice);
        vm.expectRevert(bytes("bad wager/token"));
        esc.createMatch{value: 2 ether}(id, address(0), 2 ether);
    }

    function test_RevertWhen_WrongEthAmount() public {
        bytes32 id = keccak256("m9");
        vm.prank(alice);
        vm.expectRevert(bytes("wrong ETH amount"));
        esc.createMatch{value: WAGER - 1}(id, address(0), WAGER);
    }

    // ── signatures ──

    function test_RevertWhen_InvalidSigner() public {
        bytes32 id = keccak256("s1");
        _createJoin(id, WAGER);
        vm.expectRevert(bytes("bad sig"));
        esc.submitResult(id, alice, _sign(ROGUE_KEY, id, alice));
    }

    function test_RevertWhen_SignatureForOtherWinner() public {
        bytes32 id = keccak256("s2");
        _createJoin(id, WAGER);
        // signature says alice, submission says bob
        vm.expectRevert(bytes("bad sig"));
        esc.submitResult(id, bob, _sign(SIGNER_KEY, id, alice));
    }

    function test_RevertWhen_WinnerNotPlayer() public {
        bytes32 id = keccak256("s3");
        _createJoin(id, WAGER);
        vm.expectRevert(bytes("bad winner"));
        esc.submitResult(id, carol, _sign(SIGNER_KEY, id, carol));
    }

    function test_RevertWhen_DoubleSettle() public {
        bytes32 id = keccak256("s4");
        _createJoin(id, WAGER);
        bytes memory sig = _sign(SIGNER_KEY, id, alice);
        esc.submitResult(id, alice, sig);
        vm.expectRevert(bytes("not locked"));
        esc.submitResult(id, alice, sig);
    }

    function test_RevertWhen_MalleableSignature() public {
        bytes32 id = keccak256("s5");
        _createJoin(id, WAGER);
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19Ethereum Signed Message:\n32", keccak256(abi.encodePacked(id, alice))
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SIGNER_KEY, digest);
        // flip into the high-s half of the curve — must be rejected
        bytes32 s2 = bytes32(SECP256K1_N - uint256(s));
        uint8 v2 = v == 27 ? 28 : 27;
        vm.expectRevert(bytes("bad s"));
        esc.submitResult(id, alice, abi.encodePacked(r, s2, v2));
    }

    function test_RevertWhen_BadSigLength() public {
        bytes32 id = keccak256("s6");
        _createJoin(id, WAGER);
        vm.expectRevert(bytes("sig len"));
        esc.submitResult(id, alice, hex"deadbeef");
    }

    // ── refunds ──

    function test_RefundAfterTimeout() public {
        bytes32 id = keccak256("r1");
        _createJoin(id, WAGER);
        vm.warp(block.timestamp + esc.refundTimeout());
        vm.prank(bob);
        esc.refund(id);
        assertEq(esc.claimable(alice, address(0)), WAGER);
        assertEq(esc.claimable(bob, address(0)), WAGER);
        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Voided));
    }

    function test_RevertWhen_RefundTooEarly() public {
        bytes32 id = keccak256("r2");
        _createJoin(id, WAGER);
        vm.prank(alice);
        vm.expectRevert(bytes("too early"));
        esc.refund(id);
    }

    function test_RevertWhen_RefundByNonPlayer() public {
        bytes32 id = keccak256("r3");
        _createJoin(id, WAGER);
        vm.warp(block.timestamp + esc.refundTimeout());
        vm.prank(carol);
        vm.expectRevert(bytes("not a player"));
        esc.refund(id);
    }

    // ── fee math + snapshot ──

    function _settleWithFee(uint16 bps, uint96 wager) internal returns (uint256 fee, uint256 pot) {
        MatchEscrow e = new MatchEscrow(signer, feeRecipient, bps);
        e.setMaxWager(address(0), CAP_ETH);
        bytes32 id = keccak256(abi.encode("fee", bps, wager));
        vm.prank(alice);
        e.createMatch{value: wager}(id, address(0), wager);
        vm.prank(bob);
        e.joinMatch{value: wager}(id);
        e.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));
        pot = uint256(wager) * 2;
        fee = (pot * bps) / 10_000;
        assertEq(e.claimable(alice, address(0)), pot - fee, "winner payout");
        assertEq(e.claimable(feeRecipient, address(0)), fee, "fee accrual");
        // nothing leaks: pot is fully split between winner and fee recipient
        assertEq(e.claimable(alice, address(0)) + e.claimable(feeRecipient, address(0)), pot);
    }

    function test_FeeMath_ZeroBps() public {
        (uint256 fee,) = _settleWithFee(0, WAGER);
        assertEq(fee, 0);
    }

    function test_FeeMath_250Bps() public {
        // odd wager forces truncation in the fee division
        (uint256 fee, uint256 pot) = _settleWithFee(250, 0.0123456789 ether + 1 wei);
        assertEq(fee, (pot * 250) / 10_000);
    }

    function test_FeeMath_MaxBps() public {
        (uint256 fee, uint256 pot) = _settleWithFee(1000, WAGER);
        assertEq(fee, pot / 10);
    }

    function test_RevertWhen_ConstructorFeeTooHigh() public {
        vm.expectRevert(bytes("fee too high"));
        new MatchEscrow(signer, feeRecipient, 1001);
    }

    function test_RevertWhen_ConstructorZeroRecipient() public {
        vm.expectRevert(bytes("zero recipient"));
        new MatchEscrow(signer, address(0), 250);
    }

    function test_RevertWhen_SetFeeTooHigh() public {
        vm.expectRevert(bytes("fee too high"));
        esc.setFee(1001, feeRecipient);
    }

    function test_RevertWhen_SetFeeZeroRecipient() public {
        vm.expectRevert(bytes("zero recipient"));
        esc.setFee(250, address(0));
    }

    function test_FeeSnapshot_LaterRaiseDoesNotTouchInFlight() public {
        bytes32 id = keccak256("snap");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER); // snapshots 250 bps
        esc.setFee(1000, feeRecipient); // raise to max AFTER funds are committed
        vm.prank(bob);
        esc.joinMatch{value: WAGER}(id);
        esc.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));
        uint256 pot = uint256(WAGER) * 2;
        uint256 feeAtLock = (pot * 250) / 10_000;
        assertEq(esc.claimable(alice, address(0)), pot - feeAtLock);
        assertEq(esc.claimable(feeRecipient, address(0)), feeAtLock);
    }

    function test_FeeSnapshot_NewMatchesUseNewFee() public {
        esc.setFee(500, feeRecipient);
        bytes32 id = keccak256("snap2");
        _createJoin(id, WAGER);
        esc.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));
        uint256 pot = uint256(WAGER) * 2;
        assertEq(esc.claimable(feeRecipient, address(0)), (pot * 500) / 10_000);
    }

    // ── claims ──

    function test_RevertWhen_NothingToClaim() public {
        vm.prank(carol);
        vm.expectRevert(bytes("nothing"));
        esc.claimPayout(address(0));
    }

    function test_ClaimDrainsToZero_ETH() public {
        bytes32 id = keccak256("c1");
        _createJoin(id, WAGER);
        esc.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));
        vm.prank(alice);
        esc.claimPayout(address(0));
        vm.prank(alice);
        vm.expectRevert(bytes("nothing"));
        esc.claimPayout(address(0));
    }

    function test_FullFlow_USDC() public {
        uint96 wager = 5e6; // 5 USDC
        usdc.mint(alice, wager);
        usdc.mint(bob, wager);
        vm.prank(alice);
        usdc.approve(address(esc), wager);
        vm.prank(bob);
        usdc.approve(address(esc), wager);

        bytes32 id = keccak256("usdc");
        vm.prank(alice);
        esc.createMatch(id, address(usdc), wager);
        vm.prank(bob);
        esc.joinMatch(id);
        esc.submitResult(id, bob, _sign(SIGNER_KEY, id, bob));

        uint256 pot = uint256(wager) * 2;
        uint256 fee = (pot * 250) / 10_000;
        vm.prank(bob);
        esc.claimPayout(address(usdc));
        assertEq(usdc.balanceOf(bob), pot - fee);

        vm.prank(feeRecipient);
        esc.claimPayout(address(usdc));
        assertEq(usdc.balanceOf(feeRecipient), fee);
        assertEq(usdc.balanceOf(address(esc)), 0);
    }

    function test_RevertWhen_EthSentWithTokenAnte() public {
        uint96 wager = 5e6;
        usdc.mint(alice, wager);
        vm.prank(alice);
        usdc.approve(address(esc), wager);
        vm.prank(alice);
        vm.expectRevert(bytes("no ETH with token ante"));
        esc.createMatch{value: 1 wei}(keccak256("e1"), address(usdc), wager);
    }

    // ── reentrancy ──

    function test_RevertWhen_ReenterClaimPayout() public {
        Reenterer attacker = new Reenterer(esc);
        vm.deal(address(attacker), 1 ether);
        bytes32 id = keccak256("reenter");
        attacker.create{value: WAGER}(id);
        vm.prank(bob);
        esc.joinMatch{value: WAGER}(id);
        esc.submitResult(id, address(attacker), _sign(SIGNER_KEY, id, address(attacker)));
        // the re-entering receive() makes the ETH send fail -> whole claim reverts,
        // claimable stays intact (no double-spend)
        uint256 owed = esc.claimable(address(attacker), address(0));
        vm.expectRevert(bytes("eth send failed"));
        attacker.claim();
        assertEq(esc.claimable(address(attacker), address(0)), owed);
    }

    // ── settle (server-relayed auto-payout) ──

    function test_Settle_ETH_PushesToWinner() public {
        bytes32 id = keccak256("set1");
        _createJoin(id, WAGER);
        uint256 pot = uint256(WAGER) * 2;
        uint256 fee = (pot * 250) / 10_000;

        uint256 before = alice.balance;
        esc.settle(id, alice, _sign(SIGNER_KEY, id, alice));

        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Settled));
        assertEq(alice.balance, before + pot - fee, "winner paid directly");
        assertEq(esc.claimable(alice, address(0)), 0, "no pull balance needed");
        assertEq(esc.claimable(feeRecipient, address(0)), fee, "fee still accrues to treasury");
    }

    function test_Settle_USDC_PushesToWinner() public {
        uint96 wager = 5e6;
        usdc.mint(alice, wager);
        usdc.mint(bob, wager);
        vm.prank(alice);
        usdc.approve(address(esc), wager);
        vm.prank(bob);
        usdc.approve(address(esc), wager);

        bytes32 id = keccak256("set_usdc");
        vm.prank(alice);
        esc.createMatch(id, address(usdc), wager);
        vm.prank(bob);
        esc.joinMatch(id);

        esc.settle(id, bob, _sign(SIGNER_KEY, id, bob));
        uint256 pot = uint256(wager) * 2;
        uint256 fee = (pot * 250) / 10_000;
        assertEq(usdc.balanceOf(bob), pot - fee, "winner paid in USDC directly");
        assertEq(esc.claimable(feeRecipient, address(usdc)), fee);
    }

    function test_Settle_FallbackToClaimableWhenPushFails() public {
        RejectEther winner = new RejectEther();
        vm.deal(address(winner), WAGER);
        bytes32 id = keccak256("set_reject");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(address(winner));
        esc.joinMatch{value: WAGER}(id);

        // push to the rejecting contract fails, but settle must still succeed
        esc.settle(id, address(winner), _sign(SIGNER_KEY, id, address(winner)));
        uint256 pot = uint256(WAGER) * 2;
        uint256 fee = (pot * 250) / 10_000;
        assertEq(uint8(_status(id)), uint8(MatchEscrow.Status.Settled));
        assertEq(address(winner).balance, 0, "direct push failed");
        assertEq(esc.claimable(address(winner), address(0)), pot - fee, "funds safe in claimable");
    }

    function test_Settle_ReentrantPushFallsBackToClaimable() public {
        SettleReenterer winner = new SettleReenterer(esc);
        vm.deal(address(winner), WAGER);
        bytes32 id = keccak256("set_reenter");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER);
        vm.prank(address(winner));
        esc.joinMatch{value: WAGER}(id);

        // the receive() re-enters claimPayout, hits the guard; push fails ->
        // funds fall back to claimable, settle still succeeds, no double-spend
        esc.settle(id, address(winner), _sign(SIGNER_KEY, id, address(winner)));
        uint256 pot = uint256(WAGER) * 2;
        uint256 fee = (pot * 250) / 10_000;
        assertEq(esc.claimable(address(winner), address(0)), pot - fee);
        assertGe(address(esc).balance, pot - fee, "escrow still holds the funds");
    }

    function test_RevertWhen_SettleBadSig() public {
        bytes32 id = keccak256("set_bad");
        _createJoin(id, WAGER);
        vm.expectRevert(bytes("bad sig"));
        esc.settle(id, alice, _sign(ROGUE_KEY, id, alice));
    }

    function test_RevertWhen_SettleWinnerNotPlayer() public {
        bytes32 id = keccak256("set_np");
        _createJoin(id, WAGER);
        vm.expectRevert(bytes("bad winner"));
        esc.settle(id, carol, _sign(SIGNER_KEY, id, carol));
    }

    function test_RevertWhen_SettleNotLocked() public {
        bytes32 id = keccak256("set_nl");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id, address(0), WAGER); // created, not joined
        vm.expectRevert(bytes("not locked"));
        esc.settle(id, alice, _sign(SIGNER_KEY, id, alice));
    }

    function test_RevertWhen_SettleDouble() public {
        bytes32 id = keccak256("set_dbl");
        _createJoin(id, WAGER);
        bytes memory sig = _sign(SIGNER_KEY, id, alice);
        esc.settle(id, alice, sig);
        vm.expectRevert(bytes("not locked"));
        esc.settle(id, alice, sig);
    }

    function test_RevertWhen_SettleWhilePaused() public {
        bytes32 id = keccak256("set_paused");
        _createJoin(id, WAGER);
        esc.setPaused(true);
        vm.expectRevert(bytes("paused"));
        esc.settle(id, alice, _sign(SIGNER_KEY, id, alice));
    }

    function testFuzz_SettlePaysWinnerNetOfFee(uint96 wager, uint16 bps) public {
        wager = uint96(bound(wager, 1, CAP_ETH));
        bps = uint16(bound(bps, 0, 1000));
        esc.setFee(bps, feeRecipient);
        uint256 feeBefore = esc.claimable(feeRecipient, address(0));

        bytes32 id = keccak256(abi.encode("set_fuzz", wager, bps));
        _createJoin(id, wager);
        uint256 before = bob.balance;
        esc.settle(id, bob, _sign(SIGNER_KEY, id, bob));

        uint256 pot = uint256(wager) * 2;
        uint256 fee = (pot * bps) / 10_000;
        assertEq(bob.balance, before + pot - fee, "winner paid net of fee");
        assertEq(esc.claimable(feeRecipient, address(0)) - feeBefore, fee);
        assertGe(address(esc).balance, esc.claimable(feeRecipient, address(0)));
    }

    // ── pause ──

    function test_PauseBlocksEntryButNeverExit() public {
        bytes32 id = keccak256("p1");
        _createJoin(id, WAGER);
        bytes32 id2 = keccak256("p2");
        vm.prank(alice);
        esc.createMatch{value: WAGER}(id2, address(0), WAGER);

        esc.setPaused(true);

        vm.prank(carol);
        vm.expectRevert(bytes("paused"));
        esc.createMatch{value: WAGER}(keccak256("p3"), address(0), WAGER);

        vm.prank(carol);
        vm.expectRevert(bytes("paused"));
        esc.joinMatch{value: WAGER}(id2);

        vm.expectRevert(bytes("paused"));
        esc.submitResult(id, alice, _sign(SIGNER_KEY, id, alice));

        // players can ALWAYS exit: cancel, refund and claim work while paused
        vm.prank(alice);
        esc.cancelMatch(id2);
        vm.warp(block.timestamp + esc.refundTimeout());
        vm.prank(alice);
        esc.refund(id);
        vm.prank(alice);
        esc.claimPayout(address(0)); // cancelled ante + refunded ante
        assertEq(esc.claimable(alice, address(0)), 0);
    }

    // ── ownership ──

    function test_TwoStepOwnership() public {
        esc.transferOwnership(carol);
        assertEq(esc.owner(), address(this)); // unchanged until accepted
        assertEq(esc.pendingOwner(), carol);

        vm.prank(bob);
        vm.expectRevert(bytes("not pending owner"));
        esc.acceptOwnership();

        vm.prank(carol);
        esc.acceptOwnership();
        assertEq(esc.owner(), carol);
        assertEq(esc.pendingOwner(), address(0));

        // old owner lost admin rights
        vm.expectRevert(bytes("not owner"));
        esc.setFee(100, feeRecipient);
        vm.prank(carol);
        esc.setFee(100, feeRecipient);
        assertEq(esc.feeBps(), 100);
    }

    function test_RevertWhen_NonOwnerCallsSetters() public {
        vm.startPrank(bob);
        vm.expectRevert(bytes("not owner"));
        esc.setFee(100, feeRecipient);
        vm.expectRevert(bytes("not owner"));
        esc.setPaused(true);
        vm.expectRevert(bytes("not owner"));
        esc.setResultSigner(bob);
        vm.expectRevert(bytes("not owner"));
        esc.setMaxWager(address(0), 1);
        vm.expectRevert(bytes("not owner"));
        esc.transferOwnership(bob);
        vm.stopPrank();
    }

    // ── fuzz ──

    function testFuzz_SettleFeeMath(uint96 wager, uint16 bps) public {
        wager = uint96(bound(wager, 1, CAP_ETH));
        bps = uint16(bound(bps, 0, 1000));
        esc.setFee(bps, feeRecipient);
        uint256 feeBefore = esc.claimable(feeRecipient, address(0));

        bytes32 id = keccak256(abi.encode("fuzz", wager, bps));
        _createJoin(id, wager);
        esc.submitResult(id, bob, _sign(SIGNER_KEY, id, bob));

        uint256 pot = uint256(wager) * 2;
        uint256 fee = (pot * bps) / 10_000;
        assertEq(esc.claimable(bob, address(0)), pot - fee);
        assertEq(esc.claimable(feeRecipient, address(0)) - feeBefore, fee);
        // escrow always holds enough ETH to cover every claim
        assertGe(address(esc).balance, esc.claimable(bob, address(0)) + esc.claimable(feeRecipient, address(0)));
    }

    function testFuzz_RefundConservesAntes(uint96 wager) public {
        wager = uint96(bound(wager, 1, CAP_ETH));
        bytes32 id = keccak256(abi.encode("fuzzr", wager));
        _createJoin(id, wager);
        vm.warp(block.timestamp + esc.refundTimeout());
        vm.prank(alice);
        esc.refund(id);
        assertEq(esc.claimable(alice, address(0)) + esc.claimable(bob, address(0)), uint256(wager) * 2);
        assertGe(address(esc).balance, uint256(wager) * 2);
    }
}

// ─── invariant: contract balance always covers the sum of claimables ────────

contract EscrowHandler is Test {
    uint256 constant SIGNER_KEY = 0xA11CE;
    MatchEscrow public esc;
    address[] public actors;
    address public feeRecipient;
    uint256 nonce;

    constructor(MatchEscrow _esc, address _feeRecipient) {
        esc = _esc;
        feeRecipient = _feeRecipient;
        actors.push(makeAddr("h_alice"));
        actors.push(makeAddr("h_bob"));
        actors.push(makeAddr("h_carol"));
        for (uint256 i = 0; i < actors.length; i++) vm.deal(actors[i], 1000 ether);
    }

    function actorCount() external view returns (uint256) {
        return actors.length;
    }

    function _sign(bytes32 matchId, address winner) internal pure returns (bytes memory) {
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19Ethereum Signed Message:\n32", keccak256(abi.encodePacked(matchId, winner))
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SIGNER_KEY, digest);
        return abi.encodePacked(r, s, v);
    }

    function createJoinSettle(uint96 wager, uint8 a, uint8 b, bool firstWins) external {
        wager = uint96(bound(wager, 1, 1 ether));
        address p1 = actors[a % actors.length];
        address p2 = actors[b % actors.length];
        if (p1 == p2) return;
        bytes32 id = keccak256(abi.encode("inv", nonce++));
        vm.prank(p1);
        esc.createMatch{value: wager}(id, address(0), wager);
        vm.prank(p2);
        esc.joinMatch{value: wager}(id);
        address winner = firstWins ? p1 : p2;
        esc.submitResult(id, winner, _sign(id, winner));
    }

    function createCancel(uint96 wager, uint8 a) external {
        wager = uint96(bound(wager, 1, 1 ether));
        address p1 = actors[a % actors.length];
        bytes32 id = keccak256(abi.encode("inv", nonce++));
        vm.prank(p1);
        esc.createMatch{value: wager}(id, address(0), wager);
        vm.prank(p1);
        esc.cancelMatch(id);
    }

    function createJoinRefund(uint96 wager, uint8 a, uint8 b) external {
        wager = uint96(bound(wager, 1, 1 ether));
        address p1 = actors[a % actors.length];
        address p2 = actors[b % actors.length];
        if (p1 == p2) return;
        bytes32 id = keccak256(abi.encode("inv", nonce++));
        vm.prank(p1);
        esc.createMatch{value: wager}(id, address(0), wager);
        vm.prank(p2);
        esc.joinMatch{value: wager}(id);
        vm.warp(block.timestamp + esc.refundTimeout());
        vm.prank(p1);
        esc.refund(id);
    }

    function claim(uint8 a) external {
        address who = actors[a % actors.length];
        if (esc.claimable(who, address(0)) == 0) return;
        vm.prank(who);
        esc.claimPayout(address(0));
    }

    function adjustFee(uint16 bps) external {
        bps = uint16(bound(bps, 0, 1000));
        vm.prank(esc.owner());
        esc.setFee(bps, feeRecipient);
    }
}

contract MatchEscrowInvariantTest is StdInvariant, Test {
    MatchEscrow esc;
    EscrowHandler handler;
    address feeRecipient = makeAddr("inv_feeRecipient");

    function setUp() public {
        esc = new MatchEscrow(vm.addr(0xA11CE), feeRecipient, 250);
        esc.setMaxWager(address(0), 1 ether);
        handler = new EscrowHandler(esc, feeRecipient);
        targetContract(address(handler));
    }

    function invariant_BalanceCoversClaimables() public view {
        uint256 owed = esc.claimable(feeRecipient, address(0));
        for (uint256 i = 0; i < handler.actorCount(); i++) {
            owed += esc.claimable(handler.actors(i), address(0));
        }
        assertGe(address(esc).balance, owed, "escrow underfunded vs claimables");
    }
}
