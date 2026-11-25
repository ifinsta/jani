def chunk(items, n):
    """Split items into lists of n; the last one may be shorter."""
    out = []
    for i in range(0, len(items) - n, n):
        out.append(items[i:i + n])
    return out
