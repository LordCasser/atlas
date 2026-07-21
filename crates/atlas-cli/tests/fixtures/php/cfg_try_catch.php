<?php

function load($path) {
    try {
        if (!$path) {
            throw new RuntimeException("empty");
        }
        read_file($path);
    } catch (RuntimeException $error) {
        recover($error);
    }

    return $path;
}
