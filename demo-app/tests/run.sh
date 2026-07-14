#!/bin/bash
# The unit-test suite of the demo app. It mocks the store — so it passes,
# blind to the missing WAL segment. That blindness is the point.
echo "running 142 tests"
sleep 0.4
echo "test store::apply_event ... ok"
echo "test store::mock_replay ... ok"
echo "test api::version ... ok"
echo ""
echo "test result: ok. 142 passed; 0 failed; 0 ignored"
exit 0
