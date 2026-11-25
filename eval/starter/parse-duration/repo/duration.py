UNITS = {"h": 3600, "m": 60, "s": 1}

def parse_duration(text):
    number, unit = text[:-1], text[-1]
    return int(number) * UNITS[unit]
