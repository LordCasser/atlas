<?php

interface GreeterInterface {
    public function greet();
}

class Greeter implements GreeterInterface {
    public $name;

    public function greet() {
        return "Hello, " . $this->name;
    }
}

$g = new Greeter();
$g->name = "World";
echo $g->greet();
