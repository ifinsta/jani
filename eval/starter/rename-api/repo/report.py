from calc import add_numbers

def total(values):
    result = 0
    for v in values:
        result = add_numbers(result, v)
    return result
