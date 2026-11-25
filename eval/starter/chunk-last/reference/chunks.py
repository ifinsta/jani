def chunk(items, n):
    """Split items into lists of n; the last one may be shorter."""
    if n <= 0:
        raise ValueError("n must be positive")
    return [items[i:i + n] for i in range(0, len(items), n)]
