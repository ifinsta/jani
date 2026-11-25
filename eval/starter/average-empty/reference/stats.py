def average(values):
    if not values:
        raise ValueError("average of empty list")
    return sum(values) / len(values)
