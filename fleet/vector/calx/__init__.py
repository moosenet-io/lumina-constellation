"""Calx — Vector behavioral correction system.
Monitors loop iterations, detects anti-patterns, injects corrections.
Behavioral correction concepts adapted from getcalx/oss (archived).
"""
from .engine import CalxEngine
from .triggers import T1TestTriggers, T2StyleTriggers, T3SecurityTriggers
from .history import CalxHistory

__all__ = ['CalxEngine', 'T1TestTriggers', 'T2StyleTriggers', 'T3SecurityTriggers', 'CalxHistory']
