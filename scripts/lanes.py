"""Which lane sells what.

One installation, one active catalog, four shells in front of it. The lanes
are told apart by channel availability, not by swapping catalogs (ADR 047),
and availability resolves product **and** category — so a lane that defaults
to `WITHHELD` shows nothing at all however many of its categories are offered.

Every lane therefore **defaults to OFFERED and names what it does not sell**.
Each dataset declares the rows for its own channel and no other, which is what
keeps two generated files from both claiming the same channel/target pair —
the core refuses that, and it is right to.
"""

from __future__ import annotations

# channel -> the categories that channel owns
LANES: dict[str, list[str]] = {
    "KIOSK": ["C-BURGERS", "C-COMBOS", "C-SIDES", "C-DRINKS"],
    "KIOSK-RETAIL": [
        "R-BEBIDAS", "R-SNACKS", "R-PANADERIA", "R-FRUTAS",
        "R-CARNES", "R-LACTEOS", "R-DESPENSA", "R-HOGAR",
    ],
    "KIOSK-TIM": ["T-DESAYUNO", "T-PANADERIA", "T-CAFE", "T-FRIAS"],
    "KIOSK-SBARRO": ["S-PIZZAS", "S-REBANADAS", "S-ENSALADAS", "S-BEBIDAS"],
}

# The counter is not a lane: it is the till behind all of them, and it sells
# whatever the shop sells. It declares nothing and withholds nothing.
COUNTER = "COUNTER"


def rows_for(channel: str) -> list[dict]:
    """What this channel does not sell, as availability rows.

    Every other lane's categories, withheld. Sorted, so regenerating a dataset
    twice produces the same bytes and therefore the same content hash.
    """
    if channel not in LANES:
        raise KeyError(f"{channel} is not a lane")
    withheld = sorted(
        category
        for other, categories in LANES.items()
        if other != channel
        for category in categories
    )
    return [
        {
            "channel_ref": channel,
            "target_kind": "CATEGORY",
            "target_ref": category,
            "state": "WITHHELD",
        }
        for category in withheld
    ]
