"""A fixture for the outline tests."""

import math

LIMIT = 10


def add(a, b):
    return a + b


class Shape:
    """A shape."""

    def __init__(self, name):
        self.name = name

    @property
    def area(self) -> float:
        return 0.0


class Circle(Shape):
    def __init__(self, r):
        super().__init__("circle")
        self.r = r

    def area(self) -> float:
        return math.pi * self.r ** 2


async def fetch(url: str,
                timeout: int = 10) -> bytes:
    return b""
