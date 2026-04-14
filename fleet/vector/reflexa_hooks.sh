#!/bin/bash
REFLEXA="/usr/bin/python3 /opt/lumina-fleet/engram/reflexa.py"
reflexa_t1() { [ "${REFLEXA_AVAILABLE:-1}" = "1" ] && $REFLEXA write T1 --payload "{\"task\":\"$1\",\"result\":\"$2\"}" --project "${3:-}" 2>/dev/null & }
reflexa_t2() { [ "${REFLEXA_AVAILABLE:-1}" = "1" ] && $REFLEXA write T2 --payload "{\"task\":\"$1\",\"gate_result\":\"$2\"}" --project "${3:-}" 2>/dev/null; }
reflexa_t3() { [ "${REFLEXA_AVAILABLE:-1}" = "1" ] && $REFLEXA write T3 --payload "{\"session_id\":\"$1\",\"pass_rate\":\"$2\"}" --project "${3:-}" 2>/dev/null && $REFLEXA flush 2>/dev/null; }
reflexa_failures() { [ "${REFLEXA_AVAILABLE:-1}" = "1" ] && $REFLEXA failures 2>/dev/null; }
