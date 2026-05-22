# Golden test fixture: Python simple
# Covers: function/class definitions, call/field access, import, scope nesting

import os


class Config:
    def __init__(self, port: int, host: str):
        self.port = port
        self.host = host


def create_server(port: int) -> Config:
    config = Config(port, "localhost")
    print("created")
    return config


def main():
    server = create_server(8080)
    path = os.path.join("/tmp", "data")
    print(path)


if __name__ == "__main__":
    main()
