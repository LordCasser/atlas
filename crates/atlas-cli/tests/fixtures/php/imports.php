<?php

use App\Models\User;
require_once 'config.php';

$user = new User();
echo $user->name;
