// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// The demo contract for Breeja's EVM: one storage slot, one event, one revert.
contract Counter {
    /// Storage slot 0.
    uint256 public number;

    /// `by` is indexed, so it becomes a topic `eth_getLogs` can filter on.
    event Incremented(address indexed by, uint256 newValue);

    function increment() external {
        number += 1;
        emit Incremented(msg.sender, number);
    }

    function setNumber(uint256 newNumber) external {
        number = newNumber;
    }

    /// Always reverts with a reason, to show a failed transaction being mined.
    function fail() external pure {
        revert("Counter: nope");
    }
}
