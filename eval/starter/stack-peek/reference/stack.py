class Stack:
    def __init__(self):
        self._items = []

    def push(self, item):
        self._items.append(item)

    def pop(self):
        return self._items.pop()

    def peek(self):
        if not self._items:
            raise IndexError("peek from empty stack")
        return self._items[-1]

    def __len__(self):
        return len(self._items)
