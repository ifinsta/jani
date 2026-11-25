import sys
from calc import add_numbers

def main(argv):
    print(add_numbers(int(argv[1]), int(argv[2])))

if __name__ == "__main__":
    main(sys.argv)
