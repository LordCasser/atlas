#!/bin/bash

add() {
    echo $(($1 + $2))
}

result=$(add 1 2)
echo "$result"
