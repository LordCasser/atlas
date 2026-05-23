#!/bin/bash

greet() {
    echo "Hello, $1"
}

name="World"
greet "$name"
