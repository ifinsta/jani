import sys
from calc import add

def main(argv):
    print(add(int(argv[1]), int(argv[2])))

if __name__ == "__main__":
    main(sys.argv)
