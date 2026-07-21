<?php

function dispatch($command) {
    if ($command > 0) {
        positive();
    } elseif ($command === 0) {
        zero();
    } else {
        fallback();
    }

    foreach ([1, 2] as $item) {
        visit($item);
    }

    switch ($command) {
        case 1:
            install();
            break;
        default:
            unknown();
    }

    if ($command < 0) {
        throw new RuntimeException("negative");
    }

    return $command;
}
