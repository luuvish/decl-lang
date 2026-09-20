"""The command line's stack: a platform that refuses the size asked for gets
the next one, with the recursion limit that goes with it. CPython on Windows
admits less than 256 MiB, which no corpus driver meets on the platforms the
gate runs on."""

from __future__ import annotations

import sys

from decl.cli import STACK_SIZES, large_stack


def admitting(limit: int, asked: list[int]):
    def set_size(size: int) -> int:
        asked.append(size)
        if size >= limit:
            raise ValueError(f"size not valid: {size} bytes")
        return 0

    return set_size


def test_the_largest_stack_gets_the_full_recursion_limit() -> None:
    asked: list[int] = []
    assert large_stack(admitting(1 << 40, asked)) == 1_000_000
    assert asked == [1 << 30]


def test_a_refused_size_falls_back_to_the_next_with_a_limit_in_proportion() -> None:
    asked: list[int] = []
    limit = large_stack(admitting(1 << 28, asked))  # what CPython admits on Windows
    assert asked == [1 << 30, (1 << 28) - (1 << 16)]
    assert limit == 1_000_000 * ((1 << 28) - (1 << 16)) // (1 << 30)
    assert 200_000 < limit < 250_000


def test_no_accepted_size_keeps_the_interpreter_limit() -> None:
    asked: list[int] = []
    assert large_stack(admitting(0, asked)) == sys.getrecursionlimit()
    assert asked == list(STACK_SIZES)


def test_the_sizes_are_tried_largest_first_and_the_second_fits_windows() -> None:
    assert list(STACK_SIZES) == sorted(STACK_SIZES, reverse=True)
    assert STACK_SIZES[1] < 0x10000000  # CPython's THREAD_MAX_STACKSIZE on Windows, exclusive
