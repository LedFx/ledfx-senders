"""Shared frame input type for all sender protocols."""

from typing import TypeAlias

import numpy as np
from numpy.typing import NDArray

Frame: TypeAlias = bytes | bytearray | memoryview | NDArray[np.generic]
